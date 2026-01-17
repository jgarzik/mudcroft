export interface ColorPalette {
  background: string
  surface: string
  primary: string
  secondary: string
  text: string
  textMuted: string
  error: string
  success: string
  border: string
  highlight: string
}

export interface Typography {
  fontFamily: string
  fontFamilyMono: string
  fontSize: { sm: string; md: string; lg: string; xl: string }
}

export interface BorderStyles {
  radius: string        // "0" for pixel, "8px" for modern
  width: string
  style: 'pixel' | 'solid' | 'none'
}

export interface VisualEffects {
  scanlines: boolean
  dithering: boolean
  crtGlow: boolean
  shadows: 'none' | 'subtle' | 'strong'
}

export interface Theme {
  id: string
  name: string
  colors: ColorPalette
  typography: Typography
  borders: BorderStyles
  effects: VisualEffects
  imagePromptStyle: string  // LLM prompt fragment for room images
}
