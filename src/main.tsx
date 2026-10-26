import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { api, onCallFailure, startLivenessPing } from "./api";
import { installDragGuards } from "./dnd";
import { currentLocale, initI18n, systemLocale } from "./i18n";
import { review } from "./review";
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
// A failed backend call is a moment not to ask for a store rating.
onCallFailure(() => review.problem());

// The interface language is settled before the first render: the saved choice,
// else the system's. Rust is told too, for the messages it writes.
void (async () => {
  const pref = await api.savedLanguage().catch(() => null);
  await initI18n(pref ?? systemLocale());
  api.setLanguage(currentLocale()).catch(() => {});

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
})();
