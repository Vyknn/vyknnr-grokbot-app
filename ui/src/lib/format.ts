import type { ApprovalKind, Availability, DecisionSource } from "./types";

export function pct(p: number | null | undefined): string {
  return p == null ? "–" : `${Math.round(p * 100)}%`;
}

export function sourceText(s: DecisionSource, probability: number | null = null): string {
  switch (s.kind) {
    case "jev":
      return probability == null ? `Jev (${s.model})` : `Jev ${pct(probability)}`;
    case "fallback":
      return `ไม่ได้ใช้ Jev: ${fallbackReason(s.reason)}`;
    case "manual":
      return "ผู้ใช้เลือก";
  }
}

export function fallbackReason(r: string): string {
  if (r === "jev_not_configured") return "ยังไม่ได้ตั้ง Jev key";
  if (r.startsWith("jev_error")) return "Jev ตอบไม่ได้";
  return r;
}

export const APPROVAL_TEXT: Record<ApprovalKind, string> = {
  auto: "รันเอง (Jev ผ่าน)",
  user: "ผู้ใช้อนุมัติ",
  user_edited: "ผู้ใช้แก้แล้วรัน",
  denied: "ถูกปฏิเสธ",
};

export const RISK_TEXT: Record<string, string> = {
  read_only: "อ่านอย่างเดียว",
  modifies_files: "แก้ไขไฟล์",
  destructive: "ลบหรือทำลายข้อมูล",
  network: "ใช้เครือข่าย",
  system_change: "เปลี่ยนการตั้งค่าระบบ",
  other: "อื่น ๆ",
};

export function unavailableReason(a: Availability | undefined): string | null {
  if (a === undefined) return "กำลังตรวจสอบ";
  return a === "ready" ? null : a.unavailable;
}
