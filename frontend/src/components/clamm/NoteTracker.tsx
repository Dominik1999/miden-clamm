import type { TrackedNoteWithStatus } from "@/hooks/clamm/useNoteLifecycle";
import type { TrackedNote } from "@/lib/clamm/noteStatus";
import { NOTE_STATUS_LABELS } from "@/lib/clamm/noteStatus";

export interface NoteTrackerProps {
  notes: TrackedNoteWithStatus[];
  currentBlock: number | null;
  onReclaim: (note: TrackedNote) => Promise<unknown>;
  isBusy: boolean;
  error: string | null;
}

function shortId(id: string): string {
  return id.length > 14 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}

/**
 * Lifecycle tracker for submitted pool notes:
 * pending → filled | refunded | processed, or reclaimable → reclaimed once
 * the deadline passes without consumption.
 */
export function NoteTracker({ notes, currentBlock, onReclaim, isBusy, error }: NoteTrackerProps) {
  return (
    <div className="clamm-card">
      <h3>Submitted notes</h3>
      {currentBlock !== null && (
        <p className="clamm-hint">Chain height: {currentBlock}</p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {notes.length === 0 ? (
        <p>No pool notes submitted yet.</p>
      ) : (
        <table className="clamm-table">
          <thead>
            <tr>
              <th>Note</th>
              <th>Kind</th>
              <th>Summary</th>
              <th>Deadline</th>
              <th>Status</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {notes.map((note) => (
              <tr key={note.id} data-testid={`note-row-${note.id}`}>
                <td>
                  <code>{shortId(note.id)}</code>
                </td>
                <td>{note.kind}</td>
                <td>{note.summary}</td>
                <td>{note.deadline > 0 ? note.deadline : "—"}</td>
                <td>
                  <span className={`clamm-status clamm-status-${note.status}`}>
                    {NOTE_STATUS_LABELS[note.status]}
                  </span>
                </td>
                <td>
                  {note.status === "reclaimable" && (
                    <button
                      type="button"
                      disabled={isBusy}
                      onClick={() => void onReclaim(note)}
                    >
                      Reclaim
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="clamm-hint">
        Filled/Refunded is decided by which P2ID note the pool sent back.
        Reclaim consumes an expired, unconsumed note with your own wallet.
      </p>
    </div>
  );
}
