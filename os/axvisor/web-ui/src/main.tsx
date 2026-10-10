//! Entry point: the single place where the shell and the panels are wired.
//!
//! The shell only knows the `PanelRegistry` contract, so it does not import
//! anything under `panels/`; adding a panel is a registry line, not a shell change.

import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './shell/App'
import { panelRegistry } from './panels/registry'
import './index.css'

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App registry={panelRegistry} />
  </React.StrictMode>,
)
