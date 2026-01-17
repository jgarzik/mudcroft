import type { Theme } from './types'
import { sierraRetroTheme } from './themes/sierra-retro'
import { modernTheme } from './themes/modern'

const themes: Map<string, Theme> = new Map([
  [sierraRetroTheme.id, sierraRetroTheme],
  [modernTheme.id, modernTheme],
])

export function getTheme(id: string): Theme {
  return themes.get(id) ?? sierraRetroTheme // default to sierra-retro
}

export function listThemes(): Theme[] {
  return Array.from(themes.values())
}

export const DEFAULT_THEME_ID = 'sierra-retro'
