# DASH Video Player with Deno

A simple web app for playing DASH videos using Deno.

## Requirements

- [Deno](https://deno.land/) (version 1.30.0 or higher)

## Running the Server

1. Install Deno if you haven't already:
   ```bash
   curl -fsSL https://deno.land/x/install/install.sh | sh
   ```

2. Start the server:
   ```bash
   deno run --allow-net --allow-read server.ts
   ```

3. Open your browser to:
   ```
   http://localhost:8000
   ```

## How It Works

- The server serves `index.html` which contains a DASH.js video player
- The player loads the DASH manifest from `output/manifest.mpd`
- Video segments are served from the `output/` directory
- The server handles proper MIME types for DASH content

## File Structure

- `index.html` - Main HTML file with DASH.js player
- `output/` - Contains DASH manifest and video segments
- `server.ts` - Deno web server

## Permissions

The server requires:
- `--allow-net` - To listen on the network port
- `--allow-read` - To read the HTML and video files