export type Registration = {
  user_id: string; status: string; attempt_number: number; proxy_address: string | null;
  tx_hash: string | null; error_code: string | null;
  created_at: string; updated_at: string;
};
export type Action = {
  id: number; user_id: string; status: string; payload: unknown;
  tx_hash: string | null; error_code: string | null; error_detail: string | null;
  event_id: string; chain_key: string; service_handler_address: string;
  submitted_at: string | null; closed_at: string | null; last_checked_at: string | null;
  next_check_at: string; created_at: string; updated_at: string;
};
export type ActionPage = { actions: Action[]; next_before_id: number | null };
export type Details = { action: Action; history: { id: number; status: string; error_code: string | null; error_detail: string | null; occurred_at: string }[] };
export type Config = { inbox: string; chain: string };
export class ApiError extends Error {
  constructor(public status: number, public code: string) {
    super(({ backend_unreachable: 'Cannot reach the backend. Check that the API is running.',
      unauthorized: 'Backend authentication failed. Check BACKEND_API_KEY and restart the frontend.',
      registration_quota_exceeded: 'Registration quota reached. Try again later.',
      backend_unavailable: 'The backend database is unavailable.',
      invalid_email: 'Enter one plain ASCII email address, without a display name.',
      registration_not_found: 'No registration found.',
    } as Record<string, string>)[code] ?? `Request failed: ${code} (${status})`);
  }
}
export async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(`/api${path}`, {
    ...options, headers: { 'X-Hydro-Test': '1', ...(options.body ? { 'Content-Type': 'application/json' } : {}) },
    signal: options.signal ?? AbortSignal.timeout(20000),
  });
  const body = await response.json();
  if (!response.ok) throw new ApiError(response.status, body.error ?? 'request_failed');
  return body as T;
}
export const message = (error: unknown) => error instanceof Error ? error.message : 'Request failed.';
