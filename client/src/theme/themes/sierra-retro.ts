import type { Theme } from '../types'

export const sierraRetroTheme: Theme = {
  id: 'sierra-retro',
  name: 'Sierra Retro',
  colors: {
    background: '#000055',
    surface: '#0000aa',
    primary: '#00aaaa',
    secondary: '#aa00aa',
    text: '#ffffff',
    textMuted: '#aaaaaa',
    error: '#ff5555',
    success: '#55ff55',
    border: '#aaaaaa',
    highlight: '#ffff55',
  },
  typography: {
    fontFamily: '"Press Start 2P", "VT323", monospace',
    fontFamilyMono: '"VT323", "Courier New", monospace',
    fontSize: { sm: '10px', md: '12px', lg: '14px', xl: '18px' },
  },
  borders: {
    radius: '0',
    width: '2px',
    style: 'pixel',
  },
  effects: {
    scanlines: true,
    dithering: true,
    crtGlow: true,
    shadows: 'none',
  },
  imagePromptStyle: 'Sierra adventure game (King Quest era), 320x200 VGA, 256-color palette, dithered shading, painterly pixels, 3/4 elevated view, no people in backgrounds',
}
