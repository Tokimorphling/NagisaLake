/**
 * Job-completion alerts: a system notification plus an optional chime.
 *
 * `Notification` is secure-context gated, exactly like `crypto.subtle` and
 * `navigator.clipboard` (see platform.ts). On the plain-HTTP LAN origin used for
 * development the constructor is simply absent, so every entry point here probes
 * for it and degrades to "unsupported" instead of throwing. The in-app toast
 * remains the always-available path.
 */

export type NotifyPermission = 'unsupported' | 'default' | 'granted' | 'denied'

export function notifyPermission(): NotifyPermission {
  if (typeof window === 'undefined' || typeof window.Notification !== 'function') {
    return 'unsupported'
  }
  const permission = window.Notification.permission
  return permission === 'granted' || permission === 'denied' ? permission : 'default'
}

/** Prompts for permission. Returns the resulting state, never throws. */
export async function requestNotifyPermission(): Promise<NotifyPermission> {
  if (notifyPermission() === 'unsupported') return 'unsupported'
  try {
    const result = await window.Notification.requestPermission()
    return result === 'granted' || result === 'denied' ? result : 'default'
  } catch {
    return 'denied'
  }
}

/**
 * Shows a system notification. Returns false when it could not be shown, so the
 * caller can decide whether to fall back to an in-app toast.
 */
export function showNotification(
  title: string,
  options: { body?: string; tag?: string; onClick?: () => void },
): boolean {
  if (notifyPermission() !== 'granted') return false
  try {
    const notification = new window.Notification(title, {
      body: options.body,
      // The tag collapses repeats for the same job into one notification.
      tag: options.tag,
      icon: '/favicon.svg',
      silent: true, // The chime is played separately so it can be toggled.
    })
    if (options.onClick) {
      notification.onclick = () => {
        window.focus()
        options.onClick?.()
        notification.close()
      }
    }
    return true
  } catch {
    return false
  }
}

/* --------------------------------------------------------------- Sound */

let audioContext: AudioContext | null = null

function acquireContext(): AudioContext | null {
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  if (!Ctor) return null
  if (!audioContext || audioContext.state === 'closed') {
    try {
      audioContext = new Ctor()
    } catch {
      return null
    }
  }
  return audioContext
}

/**
 * Two-note chime, synthesised rather than shipped as an asset.
 *
 * WebAudio is not secure-context gated, but autoplay policy still requires a
 * prior user gesture in the tab; the context is created lazily and resumed on
 * each play so the first chime after an interaction works.
 */
export function playChime(kind: 'success' | 'failure' = 'success'): void {
  const context = acquireContext()
  if (!context) return
  if (context.state === 'suspended') void context.resume()

  const now = context.currentTime
  // Rising for success, falling for failure, so the outcome is audible.
  const notes = kind === 'success' ? [660, 880] : [520, 380]

  notes.forEach((frequency, index) => {
    const oscillator = context.createOscillator()
    const gain = context.createGain()
    oscillator.type = 'sine'
    oscillator.frequency.value = frequency

    const start = now + index * 0.16
    const end = start + 0.34
    // Short attack, exponential release: a chime rather than a beep.
    gain.gain.setValueAtTime(0.0001, start)
    gain.gain.exponentialRampToValueAtTime(0.16, start + 0.02)
    gain.gain.exponentialRampToValueAtTime(0.0001, end)

    oscillator.connect(gain).connect(context.destination)
    oscillator.start(start)
    oscillator.stop(end + 0.02)
  })
}
