import type { SqlResult } from "@/lib/api"

type SqlResultTableProps = {
  result: SqlResult
}

function formatCell(cell: unknown): string {
  if (cell == null) return "null"
  if (typeof cell === "object") return JSON.stringify(cell)
  return String(cell)
}

export function SqlResultTable({ result }: SqlResultTableProps) {
  return (
    <div className="overflow-auto rounded-md border">
      <table className="w-full text-left font-mono text-[11px]">
        <thead className="bg-muted/50">
          <tr>
            {result.columns.map((c) => (
              <th key={c} className="px-2 py-1.5 font-medium">
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {result.rows.map((row, i) => (
            <tr key={i} className="border-t">
              {row.map((cell, j) => (
                <td key={j} className="max-w-48 truncate px-2 py-1">
                  {formatCell(cell)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <p className="border-t px-2 py-1 text-[11px] text-muted-foreground">
        {result.rowCount} row{result.rowCount === 1 ? "" : "s"}
        {result.truncated ? " (truncated)" : ""}
      </p>
    </div>
  )
}
