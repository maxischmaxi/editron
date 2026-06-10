import React from "react";
import ReactDOM from "react-dom/client";
import "@/styles/globals.css";
import App from "@/App";
import { registerAllPanels } from "@/panels";
import { registerBuiltinCommands } from "@/core/commands/builtins";
import { initSequencePlayer } from "@/core/playback/sequencePlayer";

registerAllPanels();
registerBuiltinCommands();
initSequencePlayer();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
