import { NextRequest, NextResponse } from "next/server";
import * as path from "path";
import * as fs from "fs";

export async function GET(req: NextRequest) {
  const downloadId = req.nextUrl.searchParams.get("id");
  const requestedFilename = req.nextUrl.searchParams.get("filename");

  if (!downloadId) {
    return NextResponse.json(
      { success: false, error: "Download ID required" },
      { status: 400 }
    );
  }

  const downloadsDir = path.join(process.cwd(), "downloads");
  
  // Security: Ensure we only serve files from the downloads directory
  if (!fs.existsSync(downloadsDir)) {
    return NextResponse.json(
      { success: false, error: "Downloads directory not found" },
      { status: 404 }
    );
  }

  // Find the file - it may have been renamed by yt-dlp with the video title
  const files = fs.readdirSync(downloadsDir);
  
  // Try to find file by downloadId or requested filename
  let filePath = "";
  
  if (requestedFilename) {
    // Look for the exact file first
    const exactMatch = files.find(f => f === requestedFilename);
    if (exactMatch) {
      filePath = path.join(downloadsDir, exactMatch);
    }
  }
  
  // If not found, look for any file that might be from this download
  if (!filePath) {
    // Find most recently modified file in downloads dir
    const sortedFiles = files
      .map(f => ({
        name: f,
        path: path.join(downloadsDir, f),
        mtime: fs.statSync(path.join(downloadsDir, f)).mtime,
      }))
      .sort((a, b) => b.mtime.getTime() - a.mtime.getTime());
    
    if (sortedFiles.length > 0) {
      filePath = sortedFiles[0].path;
    }
  }

  if (!filePath || !fs.existsSync(filePath)) {
    return NextResponse.json(
      { success: false, error: "File not found" },
      { status: 404 }
    );
  }

  // Read file and return as response
  const fileBuffer = fs.readFileSync(filePath);
  const fileName = path.basename(filePath);
  
  // Determine content type
  const ext = path.extname(filePath).toLowerCase();
  const contentTypeMap: Record<string, string> = {
    ".mp4": "video/mp4",
    ".webm": "video/webm",
    ".mp3": "audio/mpeg",
    ".m4a": "audio/mp4",
    ".mkv": "video/x-matroska",
  };
  const contentType = contentTypeMap[ext] || "application/octet-stream";

  return new NextResponse(fileBuffer, {
    headers: {
      "Content-Type": contentType,
      "Content-Disposition": `attachment; filename="${fileName}"`,
      "Content-Length": fileBuffer.length.toString(),
    },
  });
}
