import { useGameStore } from '../store/gameStore'

/**
 * A payment an object asked the player to confirm (game.request_payment).
 * Nothing is charged unless the player presses Pay.
 */
export function PaymentDialog({ onAnswer }: { onAnswer: (id: string, accept: boolean) => void }) {
  const payment = useGameStore((s) => s.pendingPayment)
  if (!payment) return null

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
          <button data-testid="payment-decline" onClick={() => onAnswer(payment.id, false)}>
            Decline
          </button>
          <button data-testid="payment-accept" onClick={() => onAnswer(payment.id, true)} autoFocus>
            Pay {payment.amount} credits
          </button>
        </div>
      </div>
    </div>
  )
}
