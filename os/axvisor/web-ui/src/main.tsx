import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './shell/App'
import { panelRegistry } from './panels/registry'
import './index.css'

// The one place in the repo where the shell is wired to the panels: the shell only
// knows the PanelRegistry contract and nothing about what lives under panels/. Adding
// a panel touches neither shell/ nor this file.
ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App registry={panelRegistry} />
  </React.StrictMode>,
)
