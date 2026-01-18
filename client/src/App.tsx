import { ThemeProvider } from './theme/ThemeProvider'
import { useGameStore } from './store/gameStore'
import { useWebSocket } from './hooks/useWebSocket'
import { AuthScreen } from './components/AuthScreen'
import { UniverseSelect } from './components/UniverseSelect'
import { Terminal } from './components/Terminal'
import { RoomPanel } from './components/RoomPanel'

function GameScreen() {
  const { sendCommand } = useWebSocket()
  const logout = useGameStore((s) => s.logout)
  const username = useGameStore((s) => s.username)

  return (
    <div className="game-screen flex flex-col h-full">
      {/* Header */}
      <header
        className="flex items-center justify-between"
        style={{
          padding: '0.5rem 1rem',
          borderBottom: 'var(--border-width) solid var(--color-border)',
          background: 'var(--color-surface)',
        }}
      >
        <span className="text-highlight">HemiMUD</span>
        <div className="flex items-center gap-4">
          <span className="text-sm text-muted">{username}</span>
          <button
            onClick={logout}
            style={{
              padding: '0.25rem 0.75rem',
              fontSize: 'var(--font-size-sm)',
            }}
          >
            Logout
          </button>
        </div>
      </header>

      {/* Main Content */}
      <main
        className="flex flex-1 overflow-hidden"
        style={{ padding: '0.5rem', gap: '0.5rem' }}
      >
        {/* Left: Room Panel */}
        <div style={{ width: '40%', minWidth: '300px' }}>
          <RoomPanel />
        </div>

        {/* Right: Terminal */}
        <div style={{ flex: 1, minWidth: '400px' }}>
          <Terminal onCommand={sendCommand} />
        </div>
      </main>
    </div>
  )
}

export function App() {
  const isAuthenticated = useGameStore((s) => s.isAuthenticated)
  const universe = useGameStore((s) => s.universe)
  const themeId = useGameStore((s) => s.themeId)

  let content
  if (!isAuthenticated) {
    content = <AuthScreen />
  } else if (!universe) {
    content = <UniverseSelect />
  } else {
    content = <GameScreen />
  }

  return <ThemeProvider themeId={themeId}>{content}</ThemeProvider>
}
