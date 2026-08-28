import { useCallback, useState } from "react";

/**
 * Holds the management bearer token in React memory state only.
 *
 * The token is intentionally NOT persisted to `localStorage`/`sessionStorage`:
 * a single page refresh drops it (operator must re-paste), which keeps the
 * blast radius of any XSS small — a stolen token cannot survive a reload and
 * cannot be read from disk by another origin.
 */
export function useToken(): [string, (token: string) => void, boolean, (v: boolean) => void] {
  const [token, setToken] = useState("");
  const [show, setShow] = useState(false);
  const update = useCallback((value: string) => setToken(value.trim()), []);
  return [token, update, show, setShow];
}
