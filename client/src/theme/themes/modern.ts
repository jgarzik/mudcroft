import type { Theme } from '../types'

export const modernTheme: Theme = {
  id: 'modern',
  name: 'Modern',
  colors: {
    background: '#1a1a2e',
    surface: '#16213e',
    primary: '#0f4c75',
    secondary: '#3282b8',
    text: '#ffffff',
    textMuted: '#a0a0a0',
    error: '#e74c3c',
    success: '#2ecc71',
    border: '#3282b8',
    highlight: '#bbe1fa',
  },
  typography: {
    fontFamily: '"Inter", "Segoe UI", sans-serif',
    fontFamilyMono: '"JetBrains Mono", "Fira Code", monospace',
    fontSize: { sm: '12px', md: '14px', lg: '16px', xl: '20px' },
  },
  borders: {
    radius: '8px',
    width: '1px',
    style: 'solid',
  },
  effects: {
    scanlines: false,
    dithering: false,
    crtGlow: false,
    shadows: 'subtle',
  },
  imagePromptStyle: 'Clean digital art, vibrant colors, subtle gradients, modern fantasy illustration, atmospheric lighting, professional game concept art',
}
