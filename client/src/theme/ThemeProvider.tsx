import React, { createContext, useContext, useEffect, useMemo } from 'react'
import type { Theme } from './types'
import { getTheme, DEFAULT_THEME_ID } from './themeRegistry'

interface ThemeContextValue {
  theme: Theme
  themeId: string
}

const ThemeContext = createContext<ThemeContextValue | null>(null)

export function useTheme(): ThemeContextValue {
  const context = useContext(ThemeContext)
  if (!context) {
    throw new Error('useTheme must be used within ThemeProvider')
  }
  return context
}

interface ThemeProviderProps {
  themeId: string
  children: React.ReactNode
}

export function ThemeProvider({ themeId, children }: ThemeProviderProps) {
  const theme = useMemo(() => getTheme(themeId), [themeId])

  // Apply CSS variables when theme changes
  useEffect(() => {
    const root = document.documentElement

    // Colors
    root.style.setProperty('--color-background', theme.colors.background)
    root.style.setProperty('--color-surface', theme.colors.surface)
    root.style.setProperty('--color-primary', theme.colors.primary)
    root.style.setProperty('--color-secondary', theme.colors.secondary)
    root.style.setProperty('--color-text', theme.colors.text)
    root.style.setProperty('--color-text-muted', theme.colors.textMuted)
    root.style.setProperty('--color-error', theme.colors.error)
    root.style.setProperty('--color-success', theme.colors.success)
    root.style.setProperty('--color-border', theme.colors.border)
    root.style.setProperty('--color-highlight', theme.colors.highlight)

    // Typography
    root.style.setProperty('--font-family', theme.typography.fontFamily)
    root.style.setProperty('--font-family-mono', theme.typography.fontFamilyMono)
    root.style.setProperty('--font-size-sm', theme.typography.fontSize.sm)
    root.style.setProperty('--font-size-md', theme.typography.fontSize.md)
    root.style.setProperty('--font-size-lg', theme.typography.fontSize.lg)
    root.style.setProperty('--font-size-xl', theme.typography.fontSize.xl)

    // Borders
    root.style.setProperty('--border-radius', theme.borders.radius)
    root.style.setProperty('--border-width', theme.borders.width)

    // Effects flags as data attributes for CSS
    root.dataset.scanlines = String(theme.effects.scanlines)
    root.dataset.dithering = String(theme.effects.dithering)
    root.dataset.crtGlow = String(theme.effects.crtGlow)
    root.dataset.shadows = theme.effects.shadows
    root.dataset.borderStyle = theme.borders.style
  }, [theme])

  const value = useMemo(() => ({ theme, themeId }), [theme, themeId])

  return (
    <ThemeContext.Provider value={value}>
      {children}
    </ThemeContext.Provider>
  )
}

export { DEFAULT_THEME_ID }
