import { createContext, useContext } from "react";

export type Layout = { w: number; h: number; vertical: boolean };
export const LayoutContext = createContext<Layout>({ w: 1920, h: 1080, vertical: false });
export const useLayout = () => {
  const l = useContext(LayoutContext);
  /** pick a landscape or vertical value */
  const pick = <T,>(land: T, vert: T): T => (l.vertical ? vert : land);
  return { ...l, pick };
};
