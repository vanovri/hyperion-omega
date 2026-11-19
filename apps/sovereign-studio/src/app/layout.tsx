import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Sovereign Workstation | HYPERION OMEGA Channel 20",
  description:
    "2nd-Generation 3D WebGL2 GLSL Volatility Surface, 5-Node Raft + eBPF Topology, Client-Side ZK Solvency Verifier, & Wasm/Arrow Quant Workbench.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
