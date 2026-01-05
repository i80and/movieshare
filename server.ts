import { serve } from "@std/http";
import { serveDir } from "@std/http/file-server";

// Requires --allow-read for your public directory
serve((req) =>
  serveDir(req, {
    fsRoot: "public",
    showDirListing: true,
    enableCors: true, // Adds basic Access-Control-* headers
    urlRoot: "/static",
  }),
);
