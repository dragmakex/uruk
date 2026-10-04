import type { Metadata } from "next";
import { RunForm } from "@/components/RunForm";
import { ROLE_ORDER, ROLES } from "@/lib/roles";

export const metadata: Metadata = { title: "New run" };

/**
 * The run specification page: the form on the left, the factual agent
 * legend on the right, exactly as the reference lays it out.
 */
export default function NewRunPage() {
  return (
    <div className="canvas">
      <div className="spec-layout">
        <RunForm />
        <aside aria-label="Agents on this run">
          <table className="legend-table panel">
            <thead>
              <tr>
                <th colSpan={2}>Agents on this run</th>
              </tr>
            </thead>
            <tbody>
              {ROLE_ORDER.map((role) => {
                const meta = ROLES[role];
                if (!meta) return null;
                return (
                  <tr key={role}>
                    <td>{meta.label}</td>
                    <td>{meta.description}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </aside>
      </div>
    </div>
  );
}
