import { useEffect } from "react";

/** Modal shell: Escape or a click on the backdrop cancels. */
export function ModalFrame({ children, onCancel, wide }: { children: React.ReactNode; onCancel: () => void; wide?: boolean }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCancel();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);
  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className={`dialog ${wide ? "wide" : ""}`} role="dialog" aria-modal>
        {children}
      </div>
    </div>
  );
}
