import { GITHUB_URL, SITE_URL } from "./links";

// The commands live as real files under install/, which CI runs.
import aptInstall from "../../install/apt.install.sh?raw";
import aptUninstall from "../../install/apt.uninstall.sh?raw";
import sourceInstall from "../../install/source.install.sh?raw";
import sourceUninstall from "../../install/source.uninstall.sh?raw";

export const CLI_VERSION = __CLI_VERSION__;
export const RELEASES_URL = `${GITHUB_URL}/releases`;
export const RELEASE_NOTES_URL = `${RELEASES_URL}/tag/${CLI_VERSION}`;

export type Method = {
    /// The label in the second row of tabs.
    label: string;
    title: string;
    note: string;
    shell: string;
    install: string;
    uninstall: string;
    /// What uninstalling removes, besides the binary.
    uninstallExtra?: string;
    script?: string;
};

// The one-line installers, served from public/.
const UNIX_SCRIPT: Method = {
    label: "Script",
    title: "Install Script",
    note: "Downloads the build for your system, checks it and installs it to /usr/local/bin, asking for sudo if needed. Running it again updates fsend.",
    shell: "sh",
    install: `curl -fsSL ${SITE_URL}/install.sh | sh`,
    uninstall: `curl -fsSL ${SITE_URL}/install.sh | sh -s -- --uninstall`,
    script: "/install.sh",
};

const WINDOWS_SCRIPT: Method = {
    label: "Script",
    title: "Install Script",
    note: "Downloads the build, checks it and installs it for your user only, adding it to PATH. Running it again updates fsend.",
    shell: "powershell",
    install: `irm ${SITE_URL}/install.ps1 | iex`,
    uninstall: `iex "& {$(irm ${SITE_URL}/install.ps1)} -Uninstall"`,
    uninstallExtra: " and the PATH entry",
    script: "/install.ps1",
};

const APT: Method = {
    label: "APT",
    title: "APT Repository",
    note: "For Debian and Ubuntu, on x86_64 and aarch64. Updates come with your normal apt upgrade.",
    shell: "bash",
    install: aptInstall.trim(),
    uninstall: aptUninstall.trim(),
    uninstallExtra: ", the repository entry and the signing key",
};

const CARGO: Method = {
    label: "Cargo",
    title: "Cargo",
    note: "Builds from source with the Rust toolchain and installs to ~/.cargo/bin.",
    shell: "bash",
    install: sourceInstall.trim(),
    uninstall: sourceUninstall.trim(),
};

/// An archive of the latest release, which the install scripts also fetch.
const archive = (label: string, target: string, ext = "tar.gz") => ({
    label,
    file: `fsend-${target}.${ext}`,
    url: `${RELEASES_URL}/latest/download/fsend-${target}.${ext}`,
});

export type Os = {
    id: "linux" | "macos" | "windows";
    label: string;
    methods: Method[];
    downloads: ReturnType<typeof archive>[];
};

export const OSES: Os[] = [
    {
        id: "linux",
        label: "Linux",
        methods: [UNIX_SCRIPT, APT, CARGO],
        downloads: [
            archive("x86_64", "x86_64-unknown-linux-musl"),
            archive("aarch64", "aarch64-unknown-linux-musl"),
        ],
    },
    {
        id: "macos",
        label: "macOS",
        methods: [UNIX_SCRIPT, CARGO],
        downloads: [
            archive("Apple Silicon", "aarch64-apple-darwin"),
            archive("Intel", "x86_64-apple-darwin"),
        ],
    },
    {
        id: "windows",
        label: "Windows",
        methods: [WINDOWS_SCRIPT, CARGO],
        downloads: [archive("x86_64", "x86_64-pc-windows-msvc", "zip")],
    },
];

export const USAGE = [
    { label: "Send", command: "fsend send file.txt folder/" },
    { label: "Receive", command: "fsend receive K7Q29XMV4P" },
];

export function detectOs(): Os["id"] {
    const ua = `${navigator.userAgent} ${navigator.platform}`;
    if (/Windows|Win32|Win64/i.test(ua)) return "windows";
    if (/Mac|Darwin/i.test(ua)) return "macos";
    return "linux";
}
