import { NextRequest, NextResponse } from "next/server";
import { z } from "zod";
import { exec, spawn } from "child_process";
import { promisify } from "util";
import * as path from "path";
import * as fs from "fs";

const execAsync = promisify(exec);

// Path to venv yt-dlp
const YT_DLP_PATH = path.join(process.cwd(), ".venv", "bin", "yt-dlp");
const PIP_PATH = path.join(process.cwd(), ".venv", "bin", "pip");

// Function to update yt-dlp via pip
async function updateYtDlp(): Promise<void> {
  try {
    const { stdout, stderr } = await execAsync(`${PIP_PATH} install -U yt-dlp`, { timeout: 120000 });
    if (stdout) console.log("yt-dlp pip update:", stdout.trim());
    if (stderr) console.warn("yt-dlp pip update stderr:", stderr.trim());
  } catch (error) {
    console.error("Failed to update yt-dlp via pip:", error);
  }
}

// Update yt-dlp on startup
updateYtDlp();

// Update yt-dlp periodically (every 6 hours)
setInterval(updateYtDlp, 6 * 60 * 60 * 1000);

const downloadSchema = z.object({
  url: z.string().url(),
  format: z.enum(["mp4", "mp3", "webm"]),
  quality: z.string(),
  videoId: z.string().optional(),
});

// Store active downloads in memory (use Redis in production)
const activeDownloads = new Map<string, {
  ytDlpProcess: ReturnType<typeof spawn>;
  progress: number;
  filePath: string;
  status: "processing" | "completed" | "error";
  error?: string;
}>();

// Cleanup old downloads periodically
setInterval(() => {
  const now = Date.now();
  const downloadsDir = path.join(process.cwd(), "downloads");
  if (fs.existsSync(downloadsDir)) {
    const files = fs.readdirSync(downloadsDir);
    files.forEach(file => {
      const filePath = path.join(downloadsDir, file);
      const stats = fs.statSync(filePath);
      // Delete files older than 24 hours
      if (now - stats.mtimeMs > 24 * 60 * 60 * 1000) {
        fs.unlinkSync(filePath);
      }
    });
  }
}, 60 * 60 * 1000); // Check every hour

export async function POST(req: NextRequest) {
  try {
    const body = await req.json();
    const { url, format, quality } = downloadSchema.parse(body);

    const downloadId = Math.random().toString(36).substring(7);
    const downloadsDir = path.join(process.cwd(), "downloads");
    
    if (!fs.existsSync(downloadsDir)) {
      fs.mkdirSync(downloadsDir, { recursive: true });
    }

    const outputPath = path.join(downloadsDir, "%(title)s.%(ext)s");
    
    // Build yt-dlp arguments
    let formatArg: string;
    if (format === "mp3") {
      formatArg = "bestaudio[ext=m4a]/bestaudio";
    } else if (quality === "best") {
      formatArg = `best[ext=${format}]/best`;
    } else {
      const height = quality.replace(/\D/g, "");
      formatArg = `best[height<=${height}][ext=${format}]/best[height<=${height}]/best`;
    }

    const args = [
      "--no-playlist",
      "--format", formatArg,
      "--output", outputPath,
      "--progress",
      "--newline",
      ...(format === "mp3" ? [
        "--extract-audio",
        "--audio-format", "mp3",
        "--audio-quality", "0",
      ] : []),
      url,
    ];

    // Start download process using venv yt-dlp
    const ytDlpProcess = spawn(YT_DLP_PATH, args);
    
    let fileName = "";
    let currentProgress = 0;

    ytDlpProcess.stdout.on("data", (data) => {
      const output = data.toString();
      
      // Extract filename from "[download] Destination:" line
      const destMatch = output.match(/\\[download\\] Destination: (.+)/);
      if (destMatch) {
        fileName = path.basename(destMatch[1]);
      }
      
      // Extract progress percentage
      const progressMatch = output.match(/\\[download\\]\\s+(\\d+\\.\\d+)%/);
      if (progressMatch) {
        currentProgress = parseFloat(progressMatch[1]);
      }
      
      // Update active download
      const download = activeDownloads.get(downloadId);
      if (download) {
        download.progress = currentProgress;
      }
    });

    ytDlpProcess.stderr.on("data", (data) => {
      console.error(`yt-dlp stderr: ${data}`);
    });

    ytDlpProcess.on("close", (code) => {
      const download = activeDownloads.get(downloadId);
      if (download) {
        if (code === 0) {
          download.status = "completed";
          download.progress = 100;
        } else {
          download.status = "error";
          download.error = "Download failed";
        }
      }
    });

    // Store download info
    activeDownloads.set(downloadId, {
      ytDlpProcess,
      progress: 0,
      filePath: fileName || path.join(downloadsDir, `${downloadId}.${format}`),
      status: "processing",
    });

    return NextResponse.json({
      success: true,
      data: {
        downloadId,
        url,
        format,
        quality,
        status: "processing",
        estimatedTime: "30s",
        message: "Download started",
      },
    });
  } catch (error) {
    if (error instanceof z.ZodError) {
      return NextResponse.json(
        { success: false, error: error.errors[0].message },
        { status: 400 }
      );
    }

    console.error('Download error:', error);
    
    return NextResponse.json(
      { success: false, error: "Failed to start download" },
      { status: 500 }
    );
  }
}

export async function GET(req: NextRequest) {
  const downloadId = req.nextUrl.searchParams.get("id");
  
  if (!downloadId) {
    return NextResponse.json(
      { success: false, error: "Download ID required" },
      { status: 400 }
    );
  }

  const download = activeDownloads.get(downloadId);
  
  if (!download) {
    return NextResponse.json(
      { success: false, error: "Download not found" },
      { status: 404 }
    );
  }

  // If completed, provide download URL
  if (download.status === "completed") {
    const fileName = path.basename(download.filePath);
    return NextResponse.json({
      success: true,
      data: {
        id: downloadId,
        progress: 100,
        status: "completed",
        downloadUrl: `/api/download/file?id=${downloadId}&filename=${encodeURIComponent(fileName)}`,
        fileName,
      },
    });
  }

  // Return current progress
  return NextResponse.json({
    success: true,
    data: {
      id: downloadId,
      progress: Math.floor(download.progress),
      status: download.status,
      speed: "2.5 MB/s",
      error: download.error,
    },
  });
}
