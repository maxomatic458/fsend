import { STATS_URL } from "../../config";
import type { Route, StatsReport } from "../proto";

/** Report basic and anonymous telemetry */
export function reportReceived(token: string, route: Route, bytes: number): void {
    const report: StatsReport = { token, platform: "web", transport: "webrtc", route, bytes };
    navigator.sendBeacon(STATS_URL, JSON.stringify(report));
}
