import { COUNTER_ADDRESS } from "@/config";
import { ConfiguredCounter } from "./ConfiguredCounter";

export function Counter() {
  if (!COUNTER_ADDRESS) {
    return (
      <div className="card">
        <p>
          Counter address not configured — see README for deployment
          instructions.
        </p>
      </div>
    );
  }
  return <ConfiguredCounter counterAddress={COUNTER_ADDRESS} />;
}
