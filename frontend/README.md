# Run the frontend

Requires Node.js 22.12+ and the backend API running (`task backend:api`).
The frontend uses the existing backend and inbox settings from the repository `.env`.

From the repository root:

```sh
npm --prefix frontend install
npm --prefix frontend run dev
```

Open http://127.0.0.1:5173. Restart the frontend after changing `.env`.
