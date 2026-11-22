// Device tokens: proof this browser logged in to an account before, sent
// with the next login (the server renews it). One per username, kept in
// localStorage; storage may be unavailable, so every access is guarded.

const KEY_PREFIX = 'mudcroft-device:'

export function getDeviceToken(username: string): string | undefined {
  try {
    return localStorage.getItem(KEY_PREFIX + username.toLowerCase()) ?? undefined
  } catch {
    return undefined
  }
}

export function setDeviceToken(username: string, token: string | undefined) {
  try {
    if (token) localStorage.setItem(KEY_PREFIX + username.toLowerCase(), token)
  } catch {
    // Without storage the device is simply unknown next time
  }
}

export function removeDeviceToken(username: string) {
  try {
    localStorage.removeItem(KEY_PREFIX + username.toLowerCase())
  } catch {
    // Nothing to remove
  }
}
