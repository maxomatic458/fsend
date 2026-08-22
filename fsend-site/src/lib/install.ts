import { GITHUB_URL } from "./links";

// The commands live as real files under install/.
import aptInstall from "../../install/apt.install.sh?raw";
import aptUninstall from "../../install/apt.uninstall.sh?raw";
import linuxX64Install from "../../install/linux-x86_64.install.sh?raw";
import linuxX64Uninstall from "../../install/linux-x86_64.uninstall.sh?raw";
import linuxArmInstall from "../../install/linux-aarch64.install.sh?raw";
import linuxArmUninstall from "../../install/linux-aarch64.uninstall.sh?raw";
import macosArmInstall from "../../install/macos-aarch64.install.sh?raw";
import macosArmUninstall from "../../install/macos-aarch64.uninstall.sh?raw";
import macosX64Install from "../../install/macos-x86_64.install.sh?raw";
import macosX64Uninstall from "../../install/macos-x86_64.uninstall.sh?raw";
import windowsInstall from "../../install/windows-x86_64.install.ps1?raw";
import windowsUninstall from "../../install/windows-x86_64.uninstall.ps1?raw";
import sourceInstall from "../../install/source.install.sh?raw";
import sourceUninstall from "../../install/source.uninstall.sh?raw";

export const RELEASES_URL = `${GITHUB_URL}/releases/latest`;

export type Variant = {
  key: string;
  label: string;
  title: string;
  note: string;
  scriptNote: string;
  shell: string;
  install: string;
  uninstall: string;
  uninstallExtra?: string;
};

export type Os = {
  id: "linux" | "macos" | "windows" | "source";
  label: string;
  variants: Variant[];
};

const TARBALL_NOTE =
  "Installs to /usr/local/bin, which is already on PATH. Updating means repeating these commands.";

export const OSES: Os[] = [
  {
    id: "linux",
    label: "Linux",
    variants: [
      {
        key: "apt",
        label: "Debian / Ubuntu (APT)",
        title: "APT repository",
        note: "Recommended on Debian and Ubuntu. Covers both x86_64 and aarch64.",
        scriptNote:
          "Adds the signed repository, then installs the package. Updates come with your normal apt upgrade.",
        shell: "bash",
        uninstallExtra: ", the repository entry and the signing key",
        install: aptInstall.trim(),
        uninstall: aptUninstall.trim(),
      },
      {
        key: "linux-x86_64",
        label: "x86_64",
        title: "Static binary — x86_64",
        note: "Works on any distribution. Statically linked against musl.",
        scriptNote: TARBALL_NOTE,
        shell: "bash",
        uninstallExtra: " from /usr/local/bin",
        install: linuxX64Install.trim(),
        uninstall: linuxX64Uninstall.trim(),
      },
      {
        key: "linux-aarch64",
        label: "aarch64",
        title: "Static binary — aarch64",
        note: "For 64-bit ARM machines e.g. Raspberry Pi, ARM server. Statically linked against musl.",
        scriptNote: TARBALL_NOTE,
        shell: "bash",
        uninstallExtra: " from /usr/local/bin",
        install: linuxArmInstall.trim(),
        uninstall: linuxArmUninstall.trim(),
      },
    ],
  },
  {
    id: "macos",
    label: "macOS",
    variants: [
      {
        key: "macos-aarch64",
        label: "Apple silicon",
        title: "Static binary — Apple silicon",
        note: "For M-series Macs.",
        scriptNote: TARBALL_NOTE,
        shell: "zsh",
        uninstallExtra: " from /usr/local/bin",
        install: macosArmInstall.trim(),
        uninstall: macosArmUninstall.trim(),
      },
      {
        key: "macos-x86_64",
        label: "Intel",
        title: "Static binary — Intel",
        note: "For Intel Macs.",
        scriptNote: TARBALL_NOTE,
        shell: "zsh",
        uninstallExtra: " from /usr/local/bin",
        install: macosX64Install.trim(),
        uninstall: macosX64Uninstall.trim(),
      },
    ],
  },
  {
    id: "windows",
    label: "Windows",
    variants: [
      {
        key: "windows-x86_64",
        label: "x86_64",
        title: "PowerShell",
        note: "Installs for the current user only.",
        scriptNote:
          "Unpacks into your local app data folder and adds it to PATH. Open a new terminal afterwards.",
        shell: "powershell",
        uninstallExtra: " and the PATH entry",
        install: windowsInstall.trim(),
        uninstall: windowsUninstall.trim(),
      },
    ],
  },
  {
    id: "source",
    label: "From source",
    variants: [
      {
        key: "source",
        label: "cargo",
        title: "Cargo (Rust)",
        note: "Needs the Rust toolchain.",
        scriptNote: "Lands in ~/.cargo/bin, which rustup puts on PATH.",
        shell: "bash",
        uninstallExtra: " from ~/.cargo/bin",
        install: sourceInstall.trim(),
        uninstall: sourceUninstall.trim(),
      },
    ],
  },
];

export const USAGE = [
  { label: "Send", command: "fsend send file.txt folder/" },
  { label: "Receive", command: "fsend receive 123456" },
];

export function detectPlatform(): { os: Os["id"]; key: string } {
  const ua = `${navigator.userAgent || ""} ${navigator.platform || ""}`;
  const isArm = /aarch64|arm64/i.test(ua);

  if (/Windows|Win32|Win64/i.test(ua)) {
    return { os: "windows", key: "windows-x86_64" };
  }
  if (/Mac|Darwin|iPhone|iPad/i.test(ua)) {
    return { os: "macos", key: "macos-aarch64" };
  }
  if (/Linux|X11/i.test(ua) && !/Android/i.test(ua)) {
    if (/Ubuntu|Debian/i.test(ua)) return { os: "linux", key: "apt" };
    return { os: "linux", key: isArm ? "linux-aarch64" : "linux-x86_64" };
  }
  return { os: "linux", key: "apt" };
}
