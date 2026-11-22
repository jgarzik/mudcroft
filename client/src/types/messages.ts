// Server → Client messages (match websocket.rs)
export type ServerMessage =
  | { type: 'welcome'; player_id: string; theme_id: string }
  | { type: 'output'; text: string }
  | { type: 'room'; name: string; description: string; exits: string[]; contents: string[]; image_hash?: string }
  | { type: 'error'; message: string }
  | { type: 'echo'; command: string }
  | { type: 'replaced'; message: string }
  | { type: 'confirm'; id: string; text: string; amount: number }

// Client → Server messages
export type ClientMessage =
  | { type: 'command'; text: string }
  | { type: 'ping' }
  | { type: 'confirm'; id: string; accept: boolean }

// A payment the player is asked to confirm
export interface PaymentRequest {
  id: string
  text: string
  amount: number
}

// Room data structure for state
export interface RoomData {
  name: string
  description: string
  exits: string[]
  contents: string[]
  image_hash?: string
}

// Terminal message for display
export interface TerminalMessage {
  id: string
  text: string
  timestamp: number
  type: 'output' | 'command' | 'error' | 'system'
}
