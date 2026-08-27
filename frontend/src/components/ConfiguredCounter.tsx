import { useIncrementCounter } from "@/hooks/useIncrementCounter";
import "./Counter.css";

export function ConfiguredCounter({
  counterAddress,
}: {
  counterAddress: string;
}) {
  const { increment, count, isSubmitting, status, error, explorerUrl } =
    useIncrementCounter(counterAddress);

  // While an increment is in flight the button shows the current step; otherwise
  // it shows the on-chain count and clicking it runs the increment (publish note
  // -> counter consumes it). Disabled until the count has been read.
  const buttonLabel = isSubmitting
    ? (status ?? "Working...")
    : `count is ${count ?? "..."}`;

  return (
    <div className="card">
      <button
        className="counter-button"
        onClick={increment}
        disabled={isSubmitting || count === null}
      >
        {buttonLabel}
      </button>
      <p>
        <a
          href={explorerUrl}
          target="_blank"
          rel="noreferrer"
          className="account-id"
        >
          Counter: {counterAddress}
        </a>
      </p>
      {error && <p className="error">{error}</p>}
    </div>
  );
}
