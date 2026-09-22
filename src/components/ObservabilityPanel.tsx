/**
 * 稳定的观测面板入口。
 * 运行时组件保留原名称以兼容已有调用方；新宿主壳和展示入口统一从这里导入。
 */
export {
  ObservabilityPanelView,
  RuntimeObservabilityPanel as ObservabilityPanel,
} from "./RuntimeObservabilityPanel";
export type {
  ObservabilityPanelViewProps,
  RuntimeObservabilityLabels,
  RuntimeObservabilityPanelProps,
} from "./RuntimeObservabilityPanel";
