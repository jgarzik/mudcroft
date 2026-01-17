import React, { useState, useRef, useEffect } from 'react'
import { useGameStore } from '../store/gameStore'
import type { TerminalMessage } from '../types/messages'

interface TerminalProps {
  onCommand: (command: string) => void
}

export function Terminal({ onCommand }: TerminalProps) {
  const [input, setInput] = useState('')
  const [history, setHistory] = useState<string[]>([])
  const [historyIndex, setHistoryIndex] = useState(-1)
  const messagesEndRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  const messages = useGameStore((s) => s.messages)
  const connectionStatus = useGameStore((s) => s.connectionStatus)

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  // Focus input on mount and click
  useEffect(() => {
    inputRef.current?.focus()
  }, [])

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    const trimmed = input.trim()
    if (!trimmed) return

    onCommand(trimmed)
    setHistory((prev) => [...prev, trimmed])
    setHistoryIndex(-1)
    setInput('')
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowUp') {
      e.preventDefault()
      if (history.length === 0) return
      const newIndex = historyIndex === -1 ? history.length - 1 : Math.max(0, historyIndex - 1)
      setHistoryIndex(newIndex)
      setInput(history[newIndex])
    } else if (e.key === 'ArrowDown') {
      e.preventDefault()
      if (historyIndex === -1) return
      const newIndex = historyIndex + 1
      if (newIndex >= history.length) {
        setHistoryIndex(-1)
        setInput('')
      } else {
        setHistoryIndex(newIndex)
        setInput(history[newIndex])
      }
    }
  }

  const getMessageStyle = (msg: TerminalMessage): React.CSSProperties => {
    switch (msg.type) {
      case 'command':
        return { color: 'var(--color-highlight)' }
      case 'error':
        return { color: 'var(--color-error)' }
      case 'system':
        return { color: 'var(--color-text-muted)', fontStyle: 'italic' }
      default:
        return {}
    }
  }

  return (
    <div
      className="terminal panel flex flex-col"
      style={{ height: '100%' }}
      onClick={() => inputRef.current?.focus()}
    >
      {/* Header */}
      <div
        className="terminal-header flex items-center justify-between"
        style={{
          padding: '0.5rem 0.75rem',
          borderBottom: 'var(--border-width) solid var(--color-border)',
        }}
      >
        <span className="text-sm text-muted">Terminal</span>
        <div className="flex items-center">
          <span className={`connection-status ${connectionStatus}`} />
          <span className="text-sm text-muted">{connectionStatus}</span>
        </div>
      </div>

      {/* Messages */}
      <div
        className="terminal-messages flex-1 overflow-auto font-mono"
        style={{ padding: '0.75rem' }}
      >
        {messages.map((msg) => (
          <div key={msg.id} style={{ ...getMessageStyle(msg), marginBottom: '0.25rem' }}>
            {msg.text.split('\n').map((line, i) => (
              <div key={i}>{line || '\u00A0'}</div>
            ))}
          </div>
        ))}
        <div ref={messagesEndRef} />
      </div>

      {/* Input */}
      <form
        onSubmit={handleSubmit}
        style={{
          padding: '0.5rem 0.75rem',
          borderTop: 'var(--border-width) solid var(--color-border)',
        }}
      >
        <div className="flex items-center gap-2">
          <span className="text-primary">&gt;</span>
          <input
            ref={inputRef}
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={connectionStatus === 'connected' ? 'Enter command...' : 'Connecting...'}
            disabled={connectionStatus !== 'connected'}
            className="font-mono"
            style={{
              flex: 1,
              background: 'transparent',
              border: 'none',
              outline: 'none',
              padding: 0,
            }}
          />
          <span className="cursor" />
        </div>
      </form>
    </div>
  )
}
