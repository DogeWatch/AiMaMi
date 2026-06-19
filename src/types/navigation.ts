export type Route =
  | "overview"
  | "sessions"
  | "providers"
  | "customInstructions"
  | "mcp"
  | "skills"
  | "maintenance"
  | "settings";

export const ALL_APP_ROUTES: Route[] = [
  "overview",
  "sessions",
  "providers",
  "customInstructions",
  "mcp",
  "skills",
  "maintenance",
  "settings",
];

export function isAppRoute(value: string): value is Route {
  return (ALL_APP_ROUTES as string[]).includes(value);
}
