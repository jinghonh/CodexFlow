import { useEffect, useRef, type ReactNode } from "react";

export function WorkspaceDrawer({ title, onClose, children }: {
  title: string; onClose: () => void; children: ReactNode;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const element = dialog.current!;
    const previousFocus = document.activeElement as HTMLElement | null;
    element.showModal();
    return () => { element.close(); previousFocus?.focus(); };
  }, []);
  return <dialog className="workspace-drawer" ref={dialog} aria-label={title}
    onCancel={(event) => { event.preventDefault(); close.current(); }}>
    <header className="drawer-heading"><h2>{title}</h2><button className="browse-button" onClick={onClose} aria-label={`关闭${title}`}>关闭</button></header>
    <div className="drawer-body">{children}</div>
  </dialog>;
}
