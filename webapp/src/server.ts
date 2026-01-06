// Simple web server for synchronized DASH movie watching
// Serves both the web interface and DASH files from the output directory
// Ported from Bun to Deno

const port = 3001;

// Use absolute path to ensure it works from any working directory
import { resolve } from "@std/path";
import { serveFile } from "@std/http";

const dashFilesPath = resolve(import.meta.dirname!, "../../output");

// Track connected clients and their state
interface ClientState {
  time: number;
  playing: boolean;
  lastUpdate: number;
  hasSufficientBuffer: boolean;
}

const clients = new Map<WebSocket, ClientState>();
let globalState = { time: 0, playing: false };

function resetState() {
  clients.clear();
  globalState = { time: 0, playing: false };
}

/**
 * Check if all clients have sufficient buffer
 */
function allClientsHaveSufficientBuffer(): boolean {
  for (const [, state] of clients) {
    if (!state.hasSufficientBuffer) {
      return false;
    }
  }
  return clients.size > 0; // Return true only if there are clients
}

/**
 * Reset buffer status for all clients
 */
function resetAllBufferStatuses() {
  for (const [, state] of clients) {
    state.hasSufficientBuffer = false;
  }
}

/**
 * Pause all connected clients
 */
function pauseAllClients() {
  broadcastToAll(JSON.stringify({ type: "pause" }));
  globalState.playing = false;
}

/**
 * Broadcast play command when all clients are ready
 */
function broadcastPlayWhenReady() {
  if (allClientsHaveSufficientBuffer()) {
    broadcastToAll(JSON.stringify({ type: "play" }));
    globalState.playing = true;
  } else {
    // Set up a listener for when all clients become ready
    const checkInterval = setInterval(() => {
      if (allClientsHaveSufficientBuffer()) {
        clearInterval(checkInterval);
        broadcastToAll(JSON.stringify({ type: "play" }));
        globalState.playing = true;
      }
    }, 100);
  }
}

// Create a simple HTTP server
const handler = async (request: Request): Promise<Response> => {
  const url = new URL(request.url);

  // Handle WebSocket upgrade requests
  if (request.headers.get("upgrade") === "websocket") {
    const { socket, response } = Deno.upgradeWebSocket(request);
    handleWebSocket(socket);
    return response;
  }

  // Route handling
  if (url.pathname === "/") {
    try {
      return await serveFile(request, "index.html");
    } catch (_error) {
      return new Response("File not found", { status: 404 });
    }
  } else if (url.pathname === "/dist/client.js") {
    try {
      return await serveFile(request, "dist/client.js");
    } catch (_error) {
      return new Response("File not found", { status: 404 });
    }
  } else if (url.pathname === "/dist/client.js.map") {
    try {
      return await serveFile(request, "dist/client.js.map");
    } catch (_error) {
      return new Response("File not found", { status: 404 });
    }
  } else if (url.pathname.startsWith("/output/")) {
    const filePath = url.pathname.slice(8); // Remove "/output/" prefix
    const fullPath = `${dashFilesPath}/${filePath}`;

    try {
      return await serveFile(request, fullPath);
    } catch (_error) {
      return new Response("File not found", { status: 404 });
    }
  } else {
    return new Response("Not found", { status: 404 });
  }
};

function handleWebSocket(ws: WebSocket) {
  console.log("New client connected");

  // Add client to the map immediately
  clients.set(ws, {
    time: globalState.time,
    playing: globalState.playing,
    lastUpdate: performance.now() / 1000,
    hasSufficientBuffer: false,
  });

  // Use event listeners as per the correct example
  ws.addEventListener("open", () => {
    console.log("WebSocket connection opened");
    handleNewClientConnection(ws);
  });

  function handleNewClientConnection(ws: WebSocket) {
    // If there are existing clients, pause them to allow new client to buffer
    if (clients.size > 1) {
      pauseAllClients();
    }

    // Send current global state to new client (start paused)
    ws.send(
      JSON.stringify({
        type: "init",
        time: globalState.time,
        playing: false, // Start paused to allow buffering
      }),
    );

    broadcastClientCount();
  }

  ws.addEventListener("message", (event) => {
    try {
      const data = JSON.parse(event.data);
      const clientState = clients.get(ws);

      if (!clientState) return;

      console.log("Received message:", data);

      const handlePlayEvent = (_ws: WebSocket, clientState: ClientState) => {
        clientState.playing = true;
        clientState.lastUpdate = Date.now();

        // Only broadcast play if all clients have sufficient buffer
        if (allClientsHaveSufficientBuffer()) {
          globalState.playing = true;
          broadcastToAll(JSON.stringify({ type: "play" }));
        }
        // Otherwise, the play command will be broadcast when all clients are ready
      };

      const handlePauseEvent = (clientState: ClientState) => {
        clientState.playing = false;
        clientState.lastUpdate = Date.now();
        globalState.playing = false;
        broadcastToAll(JSON.stringify({ type: "pause" }));
      };

      const handleSeekEvent = (
        _ws: WebSocket,
        clientState: ClientState,
        seekTime: number,
      ) => {
        // Pause all clients first for synchronization
        pauseAllClients();

        // Update and broadcast seek position
        clientState.time = seekTime;
        clientState.lastUpdate = Date.now();
        globalState.time = seekTime;

        // Reset buffer status for all clients
        resetAllBufferStatuses();

        // Broadcast seek to all clients
        broadcastToAll(JSON.stringify({ type: "seek", time: seekTime }));
      };

      const handleBufferReadyEvent = (clientState: ClientState) => {
        clientState.hasSufficientBuffer = true;

        console.log("all clients ready? " + allClientsHaveSufficientBuffer());

        // If we were waiting for buffer to broadcast play, check now
        if (globalState.playing && allClientsHaveSufficientBuffer()) {
          broadcastToAll(JSON.stringify({ type: "play" }));
        }
      };

      switch (data.type) {
        case "play":
          handlePlayEvent(ws, clientState);
          break;

        case "pause":
          handlePauseEvent(clientState);
          break;

        case "seek":
          handleSeekEvent(ws, clientState, data.time);
          break;

        case "bufferReady":
          handleBufferReadyEvent(clientState);
          break;

        case "ping":
          // Update last update time but don't broadcast
          clientState.lastUpdate = performance.now() / 1000;
          break;
      }
    } catch (error) {
      console.error("Error processing message:", error);
    }
  });

  ws.addEventListener("close", () => {
    console.log("Client disconnected");
    clients.delete(ws);

    if (clients.size === 0) {
      resetState();
    } else {
      broadcastClientCount();
    }
  });

  ws.addEventListener("error", (error) => {
    console.error("WebSocket error:", error);
    clients.delete(ws);
    broadcastClientCount();
  });
}

// Helper function to broadcast to all clients
function broadcastToAll(message: string) {
  console.log("Broadcasting " + message);
  for (const [client] of clients) {
    if (client.readyState === WebSocket.OPEN) {
      try {
        client.send(message);
      } catch (error) {
        console.error("Error sending to client:", error);
      }
    }
  }
}

// Helper function to broadcast client count
function broadcastClientCount() {
  const count = clients.size;
  broadcastToAll(
    JSON.stringify({
      type: "clientCount",
      count: count,
    }),
  );
}

// Heartbeat to check for stale clients
setInterval(() => {
  const now = performance.now() / 1000;
  let removedCount = 0;

  for (const [client, state] of clients) {
    if (now - state.lastUpdate > 30) {
      // 10 seconds without update
      console.log("Removing stale client");
      client.close();
      clients.delete(client);
      removedCount++;
    }
  }

  if (removedCount > 0) {
    broadcastClientCount();
  }
}, 5000); // Check every 5 seconds

// Periodic sync to keep all clients in sync
setInterval(() => {
  if (clients.size > 0) {
    // Find the most recent client state
    let latestUpdate = 0;

    for (const [, state] of clients) {
      if (state.lastUpdate > latestUpdate) {
        latestUpdate = state.lastUpdate;
      }
    }

    // // If we have recent updates, sync everyone
    // if (Date.now() - latestUpdate < 3000) {
    //   // Within last 3 seconds
    //   broadcastToAll(
    //     JSON.stringify({
    //       type: "timeUpdate",
    //       time: latestTime,
    //     }),
    //   );
    // }
  }
}, 3000); // Sync every 3 seconds

console.log(`Server running on http://localhost:${port}`);
console.log(`Serving DASH files from ${dashFilesPath}`);
console.log(`WebSocket connections will be handled automatically on all paths`);

// Start the server
Deno.serve({ port }, handler);
