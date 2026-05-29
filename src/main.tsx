import React from "react";
import ReactDOM from "react-dom/client";
import "./styles/global.css";
import App from "./App";
import { initTheme } from "./theme";

// Apply the saved theme before the first render so there's no light→dark
// flash, and start following the OS theme when the preference is "system".
initTheme();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
