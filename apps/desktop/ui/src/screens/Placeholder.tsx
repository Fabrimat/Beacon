import type { ReactNode } from "react";

export function Placeholder({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h2>{title}</h2>
      <p className="muted">{children}</p>
    </section>
  );
}
