/** @type {import('next').NextConfig} */
const isStaticExport = process.env.SOVEREIGN_STATIC_EXPORT === "1";

const nextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  ...(isStaticExport
    ? {
        output: "export",
        images: { unoptimized: true },
      }
    : {}),
};

export default nextConfig;
