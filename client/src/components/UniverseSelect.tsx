import React, { useState, useEffect } from 'react'
import { useGameStore } from '../store/gameStore'

interface Universe {
  id: string
  name: string
}

export function UniverseSelect() {
  const [universes, setUniverses] = useState<Universe[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [entering, setEntering] = useState(false)

  const setUniverse = useGameStore((s) => s.setUniverse)
  const logout = useGameStore((s) => s.logout)
  const username = useGameStore((s) => s.username)

  useEffect(() => {
    fetchUniverses()
  }, [])

  const fetchUniverses = async () => {
    try {
      const response = await fetch('/universe/list')
      if (!response.ok) {
        throw new Error('Failed to fetch universes')
      }
      const data: Universe[] = await response.json()
      setUniverses(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'An error occurred')
    } finally {
      setLoading(false)
    }
  }

  const handleSelect = (id: string) => {
    setSelectedId(id)
  }

  const handleEnter = () => {
    if (!selectedId) return
    setEntering(true)
    // Brief delay for visual feedback
    setTimeout(() => {
      setUniverse(selectedId)
    }, 300)
  }

  return (
    <div className="universe-select flex items-center justify-center h-full">
      <div
        className="panel p-4"
        style={{
          width: '420px',
          maxHeight: '80vh',
          display: 'flex',
          flexDirection: 'column',
        }}
      >
        {/* Header */}
        <div style={{ marginBottom: '1.5rem', textAlign: 'center' }}>
          <h1
            className="text-highlight"
            style={{
              fontSize: 'var(--font-size-xl)',
              marginBottom: '0.5rem',
              letterSpacing: '2px',
              textShadow: '2px 2px 0 #000055',
            }}
          >
            CHOOSE YOUR WORLD
          </h1>
          <div className="text-muted text-sm">
            Welcome, <span className="text-primary">{username}</span>
          </div>
        </div>

        {/* Universe List */}
        <div
          style={{
            flex: 1,
            overflow: 'auto',
            marginBottom: '1rem',
            border: 'var(--border-width) solid var(--color-border)',
            background: 'var(--color-background)',
            minHeight: '200px',
          }}
        >
          {loading && (
            <div
              className="flex items-center justify-center"
              style={{ padding: '2rem' }}
            >
              <span className="text-muted blink">Loading worlds...</span>
            </div>
          )}

          {error && (
            <div
              className="flex items-center justify-center"
              style={{ padding: '2rem' }}
            >
              <span className="text-error">{error}</span>
            </div>
          )}

          {!loading && !error && universes.length === 0 && (
            <div
              className="flex flex-col items-center justify-center"
              style={{ padding: '2rem', gap: '1rem' }}
            >
              <span className="text-muted">No worlds available</span>
              <span className="text-sm text-muted">
                Contact an admin to create one
              </span>
            </div>
          )}

          {!loading && !error && universes.length > 0 && (
            <ul style={{ listStyle: 'none', padding: 0, margin: 0 }}>
              {universes.map((universe, index) => (
                <li
                  key={universe.id}
                  onClick={() => handleSelect(universe.id)}
                  onDoubleClick={() => {
                    setSelectedId(universe.id)
                    handleEnter()
                  }}
                  className={`universe-item ${selectedId === universe.id ? 'selected' : ''}`}
                  style={{
                    padding: '0.75rem 1rem',
                    cursor: 'pointer',
                    borderBottom:
                      index < universes.length - 1
                        ? '1px solid var(--color-border)'
                        : 'none',
                    background:
                      selectedId === universe.id
                        ? 'var(--color-primary)'
                        : 'transparent',
                    color:
                      selectedId === universe.id
                        ? 'var(--color-background)'
                        : 'var(--color-text)',
                    transition: 'background 0.15s, color 0.15s',
                    display: 'flex',
                    alignItems: 'center',
                    gap: '0.75rem',
                  }}
                >
                  <span
                    style={{
                      color:
                        selectedId === universe.id
                          ? 'var(--color-background)'
                          : 'var(--color-highlight)',
                      fontFamily: 'var(--font-family-mono)',
                    }}
                  >
                    {selectedId === universe.id ? '>' : '\u00A0'}
                  </span>
                  <div>
                    <div style={{ fontWeight: 'bold' }}>{universe.name}</div>
                    <div
                      className="text-sm"
                      style={{
                        opacity: 0.7,
                        fontFamily: 'var(--font-family-mono)',
                      }}
                    >
                      {universe.id}
                    </div>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>

        {/* Actions */}
        <div className="flex gap-2">
          <button
            type="button"
            onClick={logout}
            style={{
              flex: 1,
              opacity: 0.8,
            }}
          >
            Logout
          </button>
          <button
            type="button"
            onClick={handleEnter}
            disabled={!selectedId || entering}
            style={{
              flex: 2,
              opacity: selectedId ? 1 : 0.5,
            }}
          >
            {entering ? 'Entering...' : 'Enter World'}
          </button>
        </div>

        {/* Hint */}
        <div
          className="text-muted text-sm"
          style={{ marginTop: '1rem', textAlign: 'center' }}
        >
          Double-click to enter directly
        </div>
      </div>

      {/* Styles */}
      <style>{`
        .universe-item:hover {
          background: var(--color-surface) !important;
        }
        .universe-item.selected:hover {
          background: var(--color-primary) !important;
        }
        .blink {
          animation: blink 1s step-end infinite;
        }
        @keyframes blink {
          50% { opacity: 0; }
        }
      `}</style>
    </div>
  )
}
