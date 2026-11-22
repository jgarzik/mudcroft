import { useEffect, useRef, useCallback } from 'react'
import { useGameStore } from '../store/gameStore'
import type { ServerMessage, ClientMessage } from '../types/messages'

const WS_URL = `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}/ws`
const PING_INTERVAL = 30000
const RECONNECT_BASE_DELAY = 1000
const RECONNECT_MAX_DELAY = 30000

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null)
  const pingIntervalRef = useRef<number | null>(null)
  const reconnectTimeoutRef = useRef<number | null>(null)
  const reconnectDelayRef = useRef(RECONNECT_BASE_DELAY)
  // Set when the server says a newer connection (another tab or device)
  // replaced this one: reconnecting would just evict that one in turn
  const replacedRef = useRef(false)

  const {
    token,
    universe,
    isAuthenticated,
    setConnectionStatus,
    addMessage,
    setRoom,
    setPlayerId,
    setThemeId,
    setPendingPayment,
  } = useGameStore()

  const cleanup = useCallback(() => {
    if (pingIntervalRef.current) {
      clearInterval(pingIntervalRef.current)
      pingIntervalRef.current = null
    }
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current)
      reconnectTimeoutRef.current = null
    }
    if (wsRef.current) {
      wsRef.current.close()
      wsRef.current = null
    }
  }, [])

  const handleMessage = useCallback((event: MessageEvent) => {
    try {
      const msg: ServerMessage = JSON.parse(event.data)

      switch (msg.type) {
        case 'welcome':
          setPlayerId(msg.player_id)
          setThemeId(msg.theme_id)
          addMessage(`Welcome! Your player ID: ${msg.player_id}`, 'system')
          break

        case 'output':
          addMessage(msg.text, 'output')
          break

        case 'room':
          setRoom({
            name: msg.name,
            description: msg.description,
            exits: msg.exits,
            contents: msg.contents,
            image_hash: msg.image_hash,
          })
          break

        case 'error':
          addMessage(msg.message, 'error')
          break

        case 'echo':
          addMessage(`> ${msg.command}`, 'command')
          break

        case 'replaced':
          replacedRef.current = true
          addMessage(msg.message, 'system')
          break

        case 'confirm':
          addMessage(msg.text, 'system')
          setPendingPayment({ id: msg.id, text: msg.text, amount: msg.amount })
          break
      }
    } catch (err) {
      console.error('Failed to parse WebSocket message:', err)
    }
  }, [addMessage, setRoom, setPlayerId, setThemeId, setPendingPayment])

  const connect = useCallback(() => {
    if (!token || !universe || !isAuthenticated) return

    cleanup()
    replacedRef.current = false
    setConnectionStatus('connecting')

    const ws = new WebSocket(`${WS_URL}?token=${encodeURIComponent(token)}&universe=${encodeURIComponent(universe)}`)
    wsRef.current = ws

    ws.onopen = () => {
      setConnectionStatus('connected')
      reconnectDelayRef.current = RECONNECT_BASE_DELAY
      addMessage('Connected to server', 'system')

      // Start ping interval
      pingIntervalRef.current = window.setInterval(() => {
        if (ws.readyState === WebSocket.OPEN) {
          const ping: ClientMessage = { type: 'ping' }
          ws.send(JSON.stringify(ping))
        }
      }, PING_INTERVAL)
    }

    ws.onmessage = handleMessage

    ws.onerror = () => {
      setConnectionStatus('error')
    }

    ws.onclose = () => {
      setConnectionStatus('disconnected')
      addMessage('Disconnected from server', 'system')

      // Schedule reconnect with exponential backoff, unless another
      // connection replaced this one
      if (!replacedRef.current && isAuthenticated && token && universe) {
        reconnectTimeoutRef.current = window.setTimeout(() => {
          reconnectDelayRef.current = Math.min(
            reconnectDelayRef.current * 2,
            RECONNECT_MAX_DELAY
          )
          connect()
        }, reconnectDelayRef.current)
      }
    }
  }, [token, universe, isAuthenticated, cleanup, setConnectionStatus, addMessage, handleMessage])

  const sendCommand = useCallback((text: string) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      const msg: ClientMessage = { type: 'command', text }
      wsRef.current.send(JSON.stringify(msg))
    }
  }, [])

  /** Answer the payment prompt (the dialog's buttons) */
  const answerPayment = useCallback((id: string, accept: boolean) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      const msg: ClientMessage = { type: 'confirm', id, accept }
      wsRef.current.send(JSON.stringify(msg))
    }
    setPendingPayment(null)
  }, [setPendingPayment])

  const disconnect = useCallback(() => {
    cleanup()
    setConnectionStatus('disconnected')
  }, [cleanup, setConnectionStatus])

  // Connect when authenticated and universe selected
  useEffect(() => {
    if (isAuthenticated && token && universe) {
      connect()
    } else {
      disconnect()
    }

    return cleanup
  }, [isAuthenticated, token, universe, connect, disconnect, cleanup])

  return { sendCommand, answerPayment, disconnect }
}
