import { createContext, useContext, useRef, type ReactNode } from "react";

type Entry = { revision: string | number; value: unknown };
type Pending = { revision: string | number; request: Promise<unknown> };

export type ProjectQueryCache = {
  load<T>(key: string, revision: string | number, query: () => Promise<T>, refresh?: boolean): Promise<T>;
  get<T>(key: string, revision: string | number): T | undefined;
  set<T>(key: string, revision: string | number, value: T): void;
};

const ProjectQueryCacheContext = createContext<ProjectQueryCache | null>(null);

function createProjectQueryCache(): ProjectQueryCache {
  const values = new Map<string, Entry>();
  const pending = new Map<string, Pending>();
  const remember = (key: string, entry: Entry) => {
    values.delete(key);
    values.set(key, entry);
    while (values.size > 32) values.delete(values.keys().next().value!);
  };

  return {
    get<T>(key: string, revision: string | number): T | undefined {
      const entry = values.get(key);
      return entry?.revision === revision ? entry.value as T : undefined;
    },
    set<T>(key: string, revision: string | number, value: T) {
      remember(key, { revision, value });
    },
    load<T>(key: string, revision: string | number, query: () => Promise<T>, refresh = false): Promise<T> {
      const value = values.get(key);
      if (!refresh && value?.revision === revision) {
        remember(key, value);
        return Promise.resolve(value.value as T);
      }

      const requestInFlight = pending.get(key);
      if (requestInFlight?.revision === revision) return requestInFlight.request as Promise<T>;

      const request = query();
      pending.set(key, { revision, request });
      request.then((next) => {
        if (pending.get(key)?.request === request) {
          remember(key, { revision, value: next });
          pending.delete(key);
        }
      }, () => {
        if (pending.get(key)?.request === request) pending.delete(key);
      });
      return request;
    },
  };
}

export function ProjectQueryCacheProvider({ children }: { children: ReactNode }) {
  const cache = useRef<ProjectQueryCache | null>(null);
  if (!cache.current) cache.current = createProjectQueryCache();
  return <ProjectQueryCacheContext.Provider value={cache.current}>{children}</ProjectQueryCacheContext.Provider>;
}

export function useProjectQueryCache(): ProjectQueryCache | null {
  return useContext(ProjectQueryCacheContext);
}

export function loadProjectQuery<T>(cache: ProjectQueryCache | null, key: string, revision: string | number,
  query: () => Promise<T>, refresh = false): Promise<T> {
  return cache ? cache.load(key, revision, query, refresh) : query();
}
