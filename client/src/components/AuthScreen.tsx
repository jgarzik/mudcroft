import React, { useState } from 'react'
import { useGameStore } from '../store/gameStore'

type AuthMode = 'login' | 'register'

export function AuthScreen() {
  const [mode, setMode] = useState<AuthMode>('login')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  const login = useGameStore((s) => s.login)

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(null)
    setLoading(true)

    try {
      const endpoint = mode === 'login' ? '/auth/login' : '/auth/register'
      const response = await fetch(endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      })

      const data = await response.json()

      if (!response.ok) {
        throw new Error(data.error || 'Authentication failed')
      }

      login(data.token, username)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'An error occurred')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="auth-screen flex items-center justify-center h-full">
      <div className="auth-container panel p-4" style={{ width: '320px' }}>
        <h1 className="text-xl text-highlight" style={{ marginBottom: '1rem', textAlign: 'center' }}>
          HemiMUD
        </h1>

        <div className="auth-tabs flex gap-2" style={{ marginBottom: '1rem' }}>
          <button
            type="button"
            onClick={() => setMode('login')}
            style={{
              flex: 1,
              opacity: mode === 'login' ? 1 : 0.6,
            }}
          >
            Login
          </button>
          <button
            type="button"
            onClick={() => setMode('register')}
            style={{
              flex: 1,
              opacity: mode === 'register' ? 1 : 0.6,
            }}
          >
            Register
          </button>
        </div>

        <form onSubmit={handleSubmit} className="flex flex-col gap-2">
          <input
            type="text"
            placeholder="Username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            required
            minLength={3}
            autoComplete="username"
          />
          <input
            type="password"
            placeholder="Password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
            minLength={6}
            autoComplete={mode === 'login' ? 'current-password' : 'new-password'}
          />

          {error && (
            <div className="text-error text-sm" style={{ padding: '0.5rem' }}>
              {error}
            </div>
          )}

          <button type="submit" disabled={loading} style={{ marginTop: '0.5rem' }}>
            {loading ? '...' : mode === 'login' ? 'Enter' : 'Create Account'}
          </button>
        </form>

        <div className="text-muted text-sm" style={{ marginTop: '1rem', textAlign: 'center' }}>
          {mode === 'login'
            ? "Don't have an account? Click Register"
            : 'Already have an account? Click Login'}
        </div>
      </div>
    </div>
  )
}
