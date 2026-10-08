import { createContext, useContext } from "react";

export const ControlLabelContext = createContext<string | undefined>(undefined);

export function useControlLabel() {
  return useContext(ControlLabelContext);
}
