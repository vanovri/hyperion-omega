import type { Metadata, Viewport } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "HYPERION // Institutional Exchange, Forensic Graph & SRE Command Terminal",
  description:
    "Sub-millisecond deterministic exchange surveillance, temporal wash-trading force graph, Chronos counterfactual replay, Chaos injection deck, and Aegis MCP SRE console.",
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  colorScheme: "dark",
  themeColor: "#06080c",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en" className="dark">
      <head>
        <meta name="color-scheme" content="dark" />
      </head>
      <body className="bg-[#06080c] text-[#f1f5f9] antialiased selection:bg-[#38bdf8]/30 selection:text-white">
        {children}
      </body>
    </html>
  );
}
