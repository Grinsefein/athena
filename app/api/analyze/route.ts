import { NextRequest, NextResponse } from "next/server";
import { z } from "zod";

const analyzeSchema = z.object({
  url: z.string().url().refine(
    (url) => url.includes("youtube.com") || url.includes("youtu.be"),
    { message: "URL must be a valid YouTube link" }
  ),
});

export async function POST(req: NextRequest) {
  try {
    const body = await req.json();
    const { url } = analyzeSchema.parse(body);

    // Mock video info for demo - in production, this would use youtube-dl-exec
    // const youtubedl = require('youtube-dl-exec');
    // const info = await youtubedl(url, { dumpSingleJson: true });
    
    // Simulated response
    const videoId = extractVideoId(url);
    const mockInfo = {
      id: videoId || "demo",
      title: "Sample YouTube Video - Amazing Content!",
      description: "This is a sample video description for demonstration purposes.",
      thumbnail: videoId 
        ? `https://i.ytimg.com/vi/${videoId}/maxresdefault.jpg`
        : "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg",
      duration: "3:45",
      author: "Awesome Channel",
      formats: [
        { format: "mp4", quality: "1080p", label: "1080p (Full HD)" },
        { format: "mp4", quality: "720p", label: "720p (HD)" },
        { format: "mp4", quality: "480p", label: "480p (SD)" },
        { format: "mp4", quality: "360p", label: "360p (Low)" },
        { format: "mp3", quality: "best", label: "MP3 Audio (Best)" },
        { format: "mp3", quality: "128k", label: "MP3 Audio (128kbps)" },
        { format: "webm", quality: "1080p", label: "WebM (1080p)" },
      ],
    };

    return NextResponse.json({ success: true, data: mockInfo });
  } catch (error) {
    if (error instanceof z.ZodError) {
      return NextResponse.json(
        { success: false, error: error.errors[0].message },
        { status: 400 }
      );
    }
    
    return NextResponse.json(
      { success: false, error: "Failed to analyze video" },
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
