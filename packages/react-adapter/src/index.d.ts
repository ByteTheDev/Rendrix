// Minimal ambient types for the adapter surface. React ships no bundled types (they live
// in `@types/react`), so this self-contained file declares the adapter-owned API plus
// loose signatures for the re-exported React surface — enough to keep `tsc --noEmit`
// quiet in consuming apps. Install `@types/react` for full-fidelity checking of your own
// components; nothing in this package changes.

export type ReactNode = unknown;
export type DependencyList = readonly unknown[];
export type EffectCallback = () => void | (() => void);
export type Dispatch<A> = (value: A | ((prev: A) => A)) => void;
export type SetStateAction<S> = S | ((prev: S) => S);
export type MutableRefObject<T> = { current: T };

export declare function render(element: ReactNode, container: Element): () => void;
export declare function flushSync<T>(fn: () => T): T;
export declare function batchedUpdates<T>(fn: () => T, ...args: unknown[]): T;
export declare function h(type: unknown, props?: Record<string, unknown> | null, ...children: ReactNode[]): unknown;
export declare function onMount(fn: () => void | (() => void)): void;
export declare function onCleanup(fn: () => void): void;

export declare function createElement(type: unknown, props?: Record<string, unknown> | null, ...children: ReactNode[]): unknown;
export declare function useState<S>(initial: S | (() => S)): [S, Dispatch<SetStateAction<S>>];
export declare function useReducer<R, I>(reducer: (prev: R, action: I) => R, initial: R): [R, Dispatch<I>];
export declare function useEffect(fn: EffectCallback, deps?: DependencyList): void;
export declare function useLayoutEffect(fn: EffectCallback, deps?: DependencyList): void;
export declare function useMemo<T>(fn: () => T, deps: DependencyList): T;
export declare function useCallback<T extends (...args: never[]) => unknown>(fn: T, deps: DependencyList): T;
export declare function useRef<T>(initial: T): MutableRefObject<T>;
export declare function useContext(ctx: unknown): unknown;
export declare function useId(): string;
export declare function createContext<T>(defaultValue: T): unknown;
export declare function memo<T>(component: T): T;
export declare function forwardRef<T>(render: T): T;
export declare function lazy<T>(load: () => Promise<{ default: T }>): T;
export declare const Fragment: unique symbol;
export declare const Suspense: unknown;
export declare const StrictMode: unknown;
