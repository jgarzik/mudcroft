import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { RoomData, TerminalMessage } from '../types/messages'
import { DEFAULT_THEME_ID } from '../theme/themeRegistry'

export type ConnectionStatus = 'disconnected' | 'connecting' | 'connected' | 'error'

interface AuthState {
  token: string | null
  username: string | null
  isAuthenticated: boolean
}

interface GameState {
  messages: TerminalMessage[]
  currentRoom: RoomData | null
  connectionStatus: ConnectionStatus
  playerId: string | null
  themeId: string
}

interface GameActions {
  // Auth
  login: (token: string, username: string) => void
  logout: () => void

  // Game
  addMessage: (text: string, type: TerminalMessage['type']) => void
  setRoom: (room: RoomData) => void
  setConnectionStatus: (status: ConnectionStatus) => void
  setPlayerId: (id: string) => void
  setThemeId: (id: string) => void
  clearMessages: () => void
}

type GameStore = AuthState & GameState & GameActions

let messageIdCounter = 0

export const useGameStore = create<GameStore>()(
  persist(
    (set) => ({
      // Auth state
      token: null,
      username: null,
      isAuthenticated: false,

      // Game state
      messages: [],
      currentRoom: null,
      connectionStatus: 'disconnected',
      playerId: null,
      themeId: DEFAULT_THEME_ID,

      // Auth actions
      login: (token, username) =>
        set({ token, username, isAuthenticated: true }),

      logout: () =>
        set({
          token: null,
          username: null,
          isAuthenticated: false,
          messages: [],
          currentRoom: null,
          connectionStatus: 'disconnected',
          playerId: null,
          themeId: DEFAULT_THEME_ID,
        }),

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

      clearMessages: () =>
        set({ messages: [] }),
    }),
    {
      name: 'hemimud-storage',
      partialize: (state) => ({
        token: state.token,
        username: state.username,
        isAuthenticated: state.isAuthenticated,
      }),
    }
  )
)
