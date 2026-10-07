import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { startLivenessPing } from "./api";
import { installDragGuards } from "./dnd";
import "./styles.css";

// Mori is a viewer, not a web page: suppress the webview's default context menu
// (our own menu is shown on files).
window.addEventListener("contextmenu", (e) => e.preventDefault());
// Drags: only Mori's own folders accept drops; nothing from outside is opened.
installDragGuards();

// Development only (removed from production builds): Ctrl+Alt+Shift+H freezes
// the page for 60 s, to exercise Rust's hung-media-engine watchdog.
if (import.meta.env.DEV) {
  window.addEventListener("keydown", (e) => {
    if (e.ctrlKey && e.altKey && e.shiftKey && e.code === "KeyH") {
      const until = Date.now() + 60_000;
      while (Date.now() < until) {
        /* simulate a wedged web content process */
      }
    }
  });
}

startLivenessPing();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
