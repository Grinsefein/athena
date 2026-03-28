import { NextRequest, NextResponse } from "next/server";
import { z } from "zod";
import { exec } from "child_process";
import { promisify } from "util";
import * as path from "path";

const execAsync = promisify(exec);

// Path to venv yt-dlp
const YT_DLP_PATH = path.join(process.cwd(), ".venv", "bin", "yt-dlp");

const analyzeSchema = z.object({
  url: z.string().url().refine(
    (url) => url.includes("youtube.com") || url.includes("youtu.be"),
    { message: "URL must be a valid YouTube link" }
  ),
});

interface YtDlpFormat {
  format_id: string;
  ext: string;
  resolution?: string;
  vcodec?: string;
  acodec?: string;
  quality?: number;
  filesize?: number;
  format_note?: string;
}

interface YtDlpInfo {
  id: string;
  title: string;
  description: string;
  thumbnail: string;
  duration: number;
  uploader: string;
  formats: YtDlpFormat[];
}

export async function POST(req: NextRequest) {
  try {
    const body = await req.json();
    const { url } = analyzeSchema.parse(body);

    // Use venv yt-dlp to fetch video info
    const { stdout } = await execAsync(
      `"${YT_DLP_PATH}" --dump-json --no-playlist "${url}"`,
      { timeout: 30000 }
    );

    const info: YtDlpInfo = JSON.parse(stdout);

    // Format duration
    const minutes = Math.floor(info.duration / 60);
    const seconds = info.duration % 60;
    const duration = `${minutes}:${seconds.toString().padStart(2, '0')}`;

    // Extract available formats
    const videoFormats = info.formats
      .filter(f => f.vcodec !== 'none' && f.resolution && f.resolution !== 'audio only')
      .map(f => ({
        format: f.ext,
        quality: f.format_note || f.resolution || 'unknown',
        label: `${f.resolution || f.format_note} (${f.ext})`,
      }));

    const audioFormats = info.formats
      .filter(f => f.acodec !== 'none' && f.vcodec === 'none')
      .map(() => ({
        format: 'mp3',
        quality: 'best',
        label: 'MP3 Audio (Best)',
      }));

    // Deduplicate and sort by quality
    const uniqueVideoFormats = Array.from(
      new Map(videoFormats.map(f => [f.label, f])).values()
    ).slice(0, 4);

    const formats = [
      ...uniqueVideoFormats,
      ...audioFormats.slice(0, 1),
    ];

    return NextResponse.json({
      success: true,
      data: {
        id: info.id,
        title: info.title,
        description: info.description.slice(0, 200) + (info.description.length > 200 ? '...' : ''),
        thumbnail: info.thumbnail,
        duration,
        author: info.uploader,
        formats,
      },
    });
  } catch (error) {
    if (error instanceof z.ZodError) {
      return NextResponse.json(
        { success: false, error: error.errors[0].message },
        { status: 400 }
      );
    }

    console.error('Analyze error:', error);
    
    return NextResponse.json(
      { success: false, error: "Failed to analyze video. Make sure the URL is valid and the video exists." },
      { status: 500 }
    );
  }
}

function extractVideoId(url: string): string | null {
  const patterns = [
    /(?:youtube\.com\/watch\?v=|youtu\.be\/|youtube\.com\/embed\/)([^&\s?]+)/,
    /youtube\.com\/shorts\/([^&\s?]+)/,
  ];
  
  for (const pattern of patterns) {
    const match = url.match(pattern);
    if (match) return match[1];
  }
  return null;
}
