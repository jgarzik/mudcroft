import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { PaymentRequest, RoomData, TerminalMessage } from '../types/messages'
import { DEFAULT_THEME_ID } from '../theme/themeRegistry'
import { getDeviceToken, removeDeviceToken } from '../deviceToken'

export type ConnectionStatus = 'disconnected' | 'connecting' | 'connected' | 'error'

interface AuthState {
  token: string | null
  username: string | null
  universe: string | null
  isAuthenticated: boolean
}

interface GameState {
  messages: TerminalMessage[]
  currentRoom: RoomData | null
  connectionStatus: ConnectionStatus
  playerId: string | null
  themeId: string
  // Payment prompts waiting for an answer, oldest first (not persisted)
  pendingPayments: PaymentRequest[]
}

interface GameActions {
  // Auth
  login: (token: string, username: string) => void
  setUniverse: (universe: string) => void
  logout: () => void

  // Game
  addMessage: (text: string, type: TerminalMessage['type']) => void
  setRoom: (room: RoomData) => void
  setConnectionStatus: (status: ConnectionStatus) => void
  setPlayerId: (id: string) => void
  setThemeId: (id: string) => void
  addPendingPayment: (payment: PaymentRequest) => void
  removePendingPayment: (id: string) => void
  clearPendingPayments: () => void
  clearMessages: () => void
}

type GameStore = AuthState & GameState & GameActions

let messageIdCounter = 0

export const useGameStore = create<GameStore>()(
  persist(
    (set, get) => ({
      // Auth state
      token: null,
      username: null,
      universe: null,
      isAuthenticated: false,

      // Game state
      messages: [],
      currentRoom: null,
      connectionStatus: 'disconnected',
      playerId: null,
      themeId: DEFAULT_THEME_ID,
      pendingPayments: [],

      // Auth actions
      login: (token, username) =>
        set({ token, username, isAuthenticated: true }),

      setUniverse: (universe) =>
        set({ universe }),

      logout: () => {
        // End the session on the server too, revoking this device's token
        const { token, username } = get()
        if (token && username) {
          const device_token = getDeviceToken(username)
          fetch('/auth/logout', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ token, device_token }),
          }).catch(() => {
            // Logged out locally either way
          })
          removeDeviceToken(username)
        }
        set({
          token: null,
          username: null,
          universe: null,
          isAuthenticated: false,
          messages: [],
          currentRoom: null,
          connectionStatus: 'disconnected',
          playerId: null,
          themeId: DEFAULT_THEME_ID,
          pendingPayments: [],
        })
      },

      // Game actions
      addMessage: (text, type) =>
        set((state) => ({
          messages: [
            ...state.messages,
            {
              id: `msg-${++messageIdCounter}`,
              text,
              timestamp: Date.now(),
              type,
            },
          ],
        })),

      setRoom: (room) =>
        set({ currentRoom: room }),

      setConnectionStatus: (status) =>
        set({ connectionStatus: status }),

      setPlayerId: (id) =>
        set({ playerId: id }),

      setThemeId: (id) =>
        set({ themeId: id }),

      addPendingPayment: (payment) =>
        set((state) => ({ pendingPayments: [...state.pendingPayments, payment] })),

      removePendingPayment: (id) =>
        set((state) => ({ pendingPayments: state.pendingPayments.filter((p) => p.id !== id) })),

      clearPendingPayments: () =>
        set({ pendingPayments: [] }),

      clearMessages: () =>
        set({ messages: [] }),
    }),
    {
      name: 'mudcroft-storage',
      partialize: (state) => ({
        token: state.token,
        username: state.username,
        isAuthenticated: state.isAuthenticated,
      }),
    }
  )
)
