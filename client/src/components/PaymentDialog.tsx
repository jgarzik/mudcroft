import { useEffect, useState } from 'react'
import { useGameStore } from '../store/gameStore'

/** Buttons stay disabled this long after a prompt opens: an Enter or click
 * meant for something else must not pay */
const ARM_DELAY_MS = 600

/**
 * A payment an object asked the player to confirm (game.request_payment).
 * Nothing is charged unless the player presses Pay. Prompts are answered
 * one at a time, oldest first.
 */
export function PaymentDialog({ onAnswer }: { onAnswer: (id: string, accept: boolean) => void }) {
  const payment = useGameStore((s) => s.pendingPayments[0] ?? null)
  const [armedFor, setArmedFor] = useState<string | null>(null)

  useEffect(() => {
    if (!payment) return
    const timer = window.setTimeout(() => setArmedFor(payment.id), ARM_DELAY_MS)
    return () => window.clearTimeout(timer)
  }, [payment])

  if (!payment) return null
  const armed = armedFor === payment.id

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="payment-text"
      data-testid="payment-dialog"
      style={{
        position: 'fixed',
        inset: 0,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        background: 'rgba(0, 0, 0, 0.6)',
        zIndex: 50,
      }}
    >
      <div
        style={{
          background: 'var(--color-surface)',
          border: 'var(--border-width) solid var(--color-border)',
          padding: '1rem 1.5rem',
          maxWidth: '28rem',
        }}
      >
        <p id="payment-text" data-testid="payment-text" style={{ marginBottom: '1rem' }}>
          {payment.text}
        </p>
        <div className="flex gap-4" style={{ justifyContent: 'flex-end' }}>
          {/* Focus starts on Decline: a stray Enter must not pay */}
          <button
            data-testid="payment-decline"
            disabled={!armed}
            onClick={() => onAnswer(payment.id, false)}
            autoFocus
          >
            Decline
          </button>
          <button data-testid="payment-accept" disabled={!armed} onClick={() => onAnswer(payment.id, true)}>
            Pay {payment.amount} credits
          </button>
        </div>
      </div>
    </div>
  )
}
