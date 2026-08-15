/**
 * Fallbacks for APIs that browsers only expose to secure contexts.
 *
 * The console is served over plain HTTP on a LAN during development, which is
 * not a secure context. Measured on Chrome at `http://192.168.x.x`:
 *
 *   isSecureContext        false
 *   crypto.subtle          unavailable  (handled in api/hash.ts)
 *   crypto.randomUUID      unavailable
 *   crypto.getRandomValues available
 *   navigator.clipboard    unavailable
 *
 * `getRandomValues` survives, so the UUID fallback keeps the same randomness
 * quality and only the formatting is hand-rolled. Clipboard has no equivalent,
 * so it degrades to the legacy `execCommand` path.
 *
 * None of this makes plain HTTP safe. It exists so LAN testing works; production
 * must use HTTPS.
 */

/** RFC 4122 version 4 UUID, using `crypto.randomUUID` when it exists. */
export function randomUuid(): string {
  const cryptoApi = globalThis.crypto
  if (typeof cryptoApi?.randomUUID === 'function') return cryptoApi.randomUUID()

  const bytes = new Uint8Array(16)
  if (typeof cryptoApi?.getRandomValues === 'function') {
    cryptoApi.getRandomValues(bytes)
  } else {
    // Last resort. Reached only on a browser without any crypto at all; the
    // value is used as an idempotency key, so uniqueness is what matters.
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256)
    }
  }
  // Version 4, variant 1.
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80

  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0'))
  return [
    hex.slice(0, 4).join(''),
    hex.slice(4, 6).join(''),
    hex.slice(6, 8).join(''),
    hex.slice(8, 10).join(''),
    hex.slice(10, 16).join(''),
  ].join('-')
}

/**
 * Copies text, falling back to a hidden textarea plus `execCommand` when the
 * async clipboard API is unavailable.
 *
 * Returns whether the copy happened, so callers can keep their "copied"
 * affordance honest instead of claiming success.
 */
export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // Permission denied or a non-focused document; try the legacy path.
    }
  }

  const textarea = document.createElement('textarea')
  textarea.value = text
  // Keep it out of view and out of the tab order, but still selectable.
  textarea.setAttribute('readonly', '')
  textarea.setAttribute('aria-hidden', 'true')
  textarea.style.position = 'fixed'
  textarea.style.top = '-9999px'
  textarea.style.opacity = '0'
  document.body.appendChild(textarea)
  try {
    textarea.select()
    textarea.setSelectionRange(0, text.length)
    return document.execCommand('copy')
  } catch {
    return false
  } finally {
    textarea.remove()
  }
}
