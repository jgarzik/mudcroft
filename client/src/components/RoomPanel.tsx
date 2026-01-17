import { useGameStore } from '../store/gameStore'
import { useTheme } from '../theme/ThemeProvider'

const API_URL = ''  // Empty for same-origin (uses Vite proxy)

export function RoomPanel() {
  const currentRoom = useGameStore((s) => s.currentRoom)
  const { theme } = useTheme()

  // Construct image URL from hash
  const imageUrl = currentRoom?.image_hash
    ? `${API_URL}/images/${currentRoom.image_hash}`
    : undefined

  if (!currentRoom) {
    return (
      <div className="room-panel panel flex items-center justify-center" style={{ height: '100%' }}>
        <div className="text-muted text-sm">
          No location data yet. Try typing "look".
        </div>
      </div>
    )
  }

  return (
    <div className="room-panel panel flex flex-col" style={{ height: '100%' }}>
      {/* Room Image */}
      {imageUrl && (
        <div
          className="room-image"
          style={{
            width: '100%',
            aspectRatio: '16/10',
            overflow: 'hidden',
            borderBottom: 'var(--border-width) solid var(--color-border)',
          }}
        >
          <img
            src={imageUrl}
            alt={currentRoom.name}
            style={{
              width: '100%',
              height: '100%',
              objectFit: 'cover',
              imageRendering: theme.effects.dithering ? 'pixelated' : 'auto',
            }}
          />
        </div>
      )}

      {/* Room Info */}
      <div style={{ padding: '0.75rem', flex: 1, overflow: 'auto' }}>
        {/* Room Name */}
        <h2
          className="text-lg text-highlight"
          style={{ marginBottom: '0.5rem' }}
        >
          {currentRoom.name}
        </h2>

        {/* Description */}
        <p style={{ marginBottom: '1rem', lineHeight: 1.6 }}>
          {currentRoom.description}
        </p>

        {/* Exits */}
        {currentRoom.exits.length > 0 && (
          <div style={{ marginBottom: '0.75rem' }}>
            <span className="text-sm text-muted">Exits: </span>
            <span className="text-primary">
              {currentRoom.exits.join(', ')}
            </span>
          </div>
        )}

        {/* Contents */}
        {currentRoom.contents.length > 0 && (
          <div>
            <span className="text-sm text-muted">You see: </span>
            <span>{currentRoom.contents.join(', ')}</span>
          </div>
        )}
      </div>
    </div>
  )
}
