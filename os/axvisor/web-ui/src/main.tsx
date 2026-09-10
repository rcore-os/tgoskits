import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './shell/App'
import { panelRegistry } from './panels/registry'
import './index.css'

// 整个仓库唯一一处「壳 与 面板」的接线点：壳只认 PanelRegistry 契约，
// 不知道 panels/ 下有什么。新增面板不需要动 shell/，也不需要动这里。
ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App registry={panelRegistry} />
  </React.StrictMode>,
)
