"use client";

import { useState, useCallback, useEffect } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ThemeToggle } from "@/components/theme-toggle";
import type { SelectRootChangeEventDetails } from "@base-ui/react/select";
import {
  Download,
  Video,
  Music,
  FileVideo,
  Check,
  Loader2,
  Link as LinkIcon,
  Play,
  Clock,
  Monitor,
  AlertCircle,
  RefreshCw,
  ExternalLink,
  Shield,
  Hd,
  Star,
} from "lucide-react";

const GithubIcon = ({ className }: { className?: string }) => (
  <svg className={className} viewBox="0 0 24 24" fill="currentColor">
    <path d="M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z"/>
  </svg>
);

const YoutubeIcon = ({ className }: { className?: string }) => (
  <svg className={className} viewBox="0 0 24 24" fill="currentColor">
    <path d="M23.498 6.186a3.016 3.016 0 0 0-2.122-2.136C19.505 3.545 12 3.545 12 3.545s-7.505 0-9.377.505A3.017 3.017 0 0 0 .502 6.186C0 8.07 0 12 0 12s0 3.93.502 5.814a3.016 3.016 0 0 0 2.122 2.136c1.871.505 9.376.505 9.376.505s7.505 0 9.377-.505a3.015 3.015 0 0 0 2.122-2.136C24 15.93 24 12 24 12s0-3.93-.502-5.814zM9.545 15.568V8.432L15.818 12l-6.273 3.568z"/>
  </svg>
);

type DownloadFormat = "mp4" | "mp3" | "webm";

interface VideoFormat {
  format: string;
  quality: string;
  label: string;
}

interface VideoInfo {
  id: string;
  title: string;
  description: string;
  thumbnail: string;
  duration: string;
  author: string;
  formats: VideoFormat[];
}

interface DownloadState {
  status: "idle" | "analyzing" | "ready" | "downloading" | "completed" | "error";
  progress: number;
  error?: string;
  downloadId?: string;
  downloadUrl?: string;
  fileName?: string;
}

const fadeInUp = {
  initial: { opacity: 0, y: 20 },
  animate: { opacity: 1, y: 0 },
  exit: { opacity: 0, y: -20 },
};

const staggerContainer = {
  animate: {
    transition: {
      staggerChildren: 0.1,
    },
  },
};

export default function Home() {
  const [url, setUrl] = useState("");
  const [format, setFormat] = useState<string>("mp4");
  const [quality, setQuality] = useState("best");
  const [videoInfo, setVideoInfo] = useState<VideoInfo | null>(null);
  const [downloadState, setDownloadState] = useState<DownloadState>({
    status: "idle",
    progress: 0,
  });
  const [recentDownloads, setRecentDownloads] = useState<string[]>([]);

  useEffect(() => {
    const saved = localStorage.getItem("athena_recent_downloads");
    if (saved) {
      setRecentDownloads(JSON.parse(saved));
    }
  }, []);

  const saveRecentDownload = useCallback((videoTitle: string) => {
    setRecentDownloads((prev) => {
      const newDownloads = [videoTitle, ...prev.slice(0, 4)];
      localStorage.setItem("athena_recent_downloads", JSON.stringify(newDownloads));
      return newDownloads;
    });
  }, []);

  const handleAnalyze = async () => {
    if (!url) return;
    
    setDownloadState({ status: "analyzing", progress: 0 });
    setVideoInfo(null);

    try {
      const response = await fetch("/api/analyze", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url }),
      });

      const data = await response.json();

      if (!data.success) {
        throw new Error(data.error || "Failed to analyze video");
      }

      setVideoInfo(data.data);
      setDownloadState({ status: "ready", progress: 0 });
    } catch (error) {
      setDownloadState({
        status: "error",
        progress: 0,
        error: error instanceof Error ? error.message : "Failed to analyze video",
      });
    }
  };

  const handleDownload = async () => {
    if (!videoInfo) return;

    setDownloadState((prev) => ({ ...prev, status: "downloading", progress: 0 }));

    try {
      const response = await fetch("/api/download", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          url,
          format,
          quality,
          videoId: videoInfo.id,
        }),
      });

      const data = await response.json();

      if (!data.success) {
        throw new Error(data.error || "Failed to start download");
      }

      const { downloadId } = data.data;

      // Poll for real progress
      const pollInterval = setInterval(async () => {
        try {
          const progressRes = await fetch(`/api/download?id=${downloadId}`);
          const progressData = await progressRes.json();

          if (!progressData.success) {
            throw new Error(progressData.error);
          }

          const { progress, status, downloadUrl, fileName } = progressData.data;

          setDownloadState((prev) => ({
            ...prev,
            progress,
            status,
            downloadUrl,
            fileName,
          }));

          if (status === "completed") {
            clearInterval(pollInterval);
            saveRecentDownload(videoInfo.title);
          } else if (status === "error") {
            clearInterval(pollInterval);
            setDownloadState((prev) => ({
              ...prev,
              status: "error",
              error: "Download failed. Please try again.",
            }));
          }
        } catch (err) {
          console.error("Progress poll error:", err);
        }
      }, 1000);

      // Cleanup interval after 5 minutes to prevent indefinite polling
      setTimeout(() => clearInterval(pollInterval), 5 * 60 * 1000);
    } catch (error) {
      setDownloadState({
        status: "error",
        progress: 0,
        error: error instanceof Error ? error.message : "Download failed",
      });
    }
  };

  const resetForm = () => {
    setUrl("");
    setVideoInfo(null);
    setDownloadState({ status: "idle", progress: 0 });
    setFormat("mp4");
    setQuality("best");
  };

  const availableFormats = videoInfo?.formats.filter((f) => f.format === format) || [];

  const features = [
    { icon: Video, title: "HD Quality", desc: "Download in 4K, 1080p, 720p" },
    { icon: Music, title: "Audio Only", desc: "Extract MP3 audio files" },
    { icon: Download, title: "Fast Downloads", desc: "High-speed parallel downloads" },
    { icon: Shield, title: "Safe & Secure", desc: "No malware, no tracking" },
  ];

  return (
    <div className="flex flex-col min-h-screen">
      <motion.header
        initial={{ y: -100 }}
        animate={{ y: 0 }}
        className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur-xl safe-area-inset-top"
      >
        <div className="container mx-auto px-3 sm:px-4 h-14 sm:h-16 flex items-center justify-between">
          <motion.div
            initial={{ opacity: 0, x: -20 }}
            animate={{ opacity: 1, x: 0 }}
            className="flex items-center gap-2 sm:gap-3"
          >
            <div className="w-8 h-8 sm:w-10 sm:h-10 bg-slate-900 dark:bg-slate-100 rounded-lg flex items-center justify-center">
              <YoutubeIcon className="w-5 h-5 sm:w-6 sm:h-6 text-white dark:text-slate-900" />
            </div>
            <div className="flex flex-col">
              <span className="text-lg sm:text-xl font-semibold text-foreground">
                Athena
              </span>
              <span className="text-[9px] sm:text-[10px] text-muted-foreground -mt-0.5 hidden sm:block">YouTube Downloader</span>
            </div>
          </motion.div>

          <nav className="hidden md:flex items-center gap-1">
            {["Home", "Features", "FAQ"].map((item, i) => (
              <motion.a
                key={item}
                href={item === "Home" ? "#" : `#${item.toLowerCase()}`}
                initial={{ opacity: 0, y: -10 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ delay: i * 0.1 }}
                className="px-4 py-2 text-sm font-medium text-muted-foreground hover:text-foreground transition-colors rounded-lg hover:bg-muted"
              >
                {item}
              </motion.a>
            ))}
          </nav>

          <motion.div
            initial={{ opacity: 0, x: 20 }}
            animate={{ opacity: 1, x: 0 }}
            className="flex items-center gap-1 sm:gap-2"
          >
            <ThemeToggle />
            <Button variant="ghost" size="icon" className="w-10 h-10 sm:w-9 sm:h-9 touch-target">
              <a href="https://github.com/Grinsefein/athena" target="_blank" rel="noopener noreferrer" className="flex items-center justify-center w-full h-full">
                <GithubIcon className="w-5 h-5 sm:w-4 sm:h-4" />
              </a>
            </Button>
          </motion.div>
        </div>
      </motion.header>

      <section className="flex-1 flex flex-col items-center justify-center py-6 sm:py-12 md:py-20 px-3 sm:px-4">
        <motion.div
          variants={staggerContainer}
          initial="initial"
          animate="animate"
          className="text-center max-w-3xl mx-auto mb-4 sm:mb-8"
        >
          <motion.h1
            variants={fadeInUp}
            className="text-2xl sm:text-4xl md:text-5xl font-semibold tracking-tight mb-3 sm:mb-6 text-foreground px-2"
          >
            YouTube Video Downloader
          </motion.h1>

          <motion.p
            variants={fadeInUp}
            className="text-base sm:text-lg md:text-xl text-muted-foreground max-w-2xl mx-auto px-2"
          >
            Download videos and audio from YouTube in your preferred format and quality.
          </motion.p>
        </motion.div>

        <motion.div
          initial={{ opacity: 0, y: 30, scale: 0.95 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ delay: 0.3, duration: 0.5 }}
          className="w-full max-w-2xl mx-auto px-1 sm:px-0"
        >
          <Card className="shadow-lg border border-slate-200 dark:border-slate-800 overflow-hidden bg-gradient-to-b from-white to-slate-50/50 dark:from-slate-900 dark:to-slate-900/50">
            <CardContent className="p-4 sm:p-6 md:p-8">
              <div className="flex flex-col gap-3 mb-4 sm:mb-6">
                <div className="relative flex-1">
                  <LinkIcon className="absolute left-3 top-1/2 -translate-y-1/2 w-5 h-5 text-muted-foreground pointer-events-none" />
                  <Input
                    placeholder="Paste YouTube URL here..."
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    className="pl-10 h-12 sm:h-14 text-base bg-muted/50 border-muted-foreground/20 touch-target-lg"
                    onKeyDown={(e) => e.key === "Enter" && handleAnalyze()}
                    disabled={downloadState.status === "analyzing"}
                    type="url"
                    inputMode="url"
                    autoCapitalize="none"
                    autoCorrect="off"
                  />
                </div>
                <Button
                  onClick={handleAnalyze}
                  disabled={!url || downloadState.status === "analyzing" || downloadState.status === "downloading"}
                  className="h-12 sm:h-14 px-6 bg-gradient-to-r from-blue-600 to-indigo-600 hover:from-blue-700 hover:to-indigo-700 text-white font-medium touch-target-lg"
                >
                  {downloadState.status === "analyzing" ? (
                    <Loader2 className="w-5 h-5 animate-spin" />
                  ) : (
                    <>
                      <Play className="w-5 h-5 mr-2" />
                      <span>Analyze</span>
                    </>
                  )}
                </Button>
              </div>

              <AnimatePresence>
                {downloadState.status === "error" && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={{ opacity: 0, height: 0 }}
                    className="mb-4 sm:mb-6 p-3 sm:p-4 bg-destructive/10 border border-destructive/20 rounded-xl flex items-start sm:items-center gap-3"
                  >
                    <AlertCircle className="w-5 h-5 text-destructive flex-shrink-0 mt-0.5 sm:mt-0" />
                    <p className="text-sm text-destructive flex-1">{downloadState.error}</p>
                    <Button variant="ghost" size="sm" onClick={() => setDownloadState({ status: "idle", progress: 0 })} className="ml-auto touch-target flex-shrink-0">
                      <RefreshCw className="w-4 h-4" />
                    </Button>
                  </motion.div>
                )}
              </AnimatePresence>

              <AnimatePresence>
                {videoInfo && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={{ opacity: 0, height: 0 }}
                    className="mb-4 sm:mb-6"
                  >
                    <div className="p-3 sm:p-4 bg-muted/50 rounded-lg border">
                      <div className="flex flex-col sm:flex-row gap-3 sm:gap-4">
                        <div className="relative w-full sm:w-40 md:w-48 h-32 sm:h-24 md:h-28 flex-shrink-0 rounded-md overflow-hidden bg-black/10">
                          <img
                            src={videoInfo.thumbnail}
                            alt={videoInfo.title}
                            className="w-full h-full object-cover"
                            loading="lazy"
                            decoding="async"
                          />
                          <div className="absolute bottom-2 right-2 px-2 py-0.5 bg-black/70 text-white text-xs rounded">
                            {videoInfo.duration}
                          </div>
                        </div>
                        <div className="flex-1 min-w-0">
                          <h3 className="font-semibold text-foreground text-sm sm:text-base line-clamp-2 mb-1">
                            {videoInfo.title}
                          </h3>
                          <p className="text-xs sm:text-sm text-muted-foreground">{videoInfo.author}</p>
                          <div className="flex items-center gap-2 mt-2">
                            <Badge variant="secondary" className="text-xs touch-target">
                              <Check className="w-3 h-3 mr-1" />
                              Ready to download
                            </Badge>
                          </div>
                        </div>
                      </div>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>

              <AnimatePresence>
                {videoInfo && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={{ opacity: 0, height: 0 }}
                    className="grid grid-cols-1 sm:grid-cols-2 gap-3 sm:gap-4 mb-4 sm:mb-6"
                  >
                    <div>
                      <label className="text-sm font-medium mb-2 block">Format</label>
                      <Select value={format} onValueChange={(v) => setFormat(v as DownloadFormat)}>
                        <SelectTrigger className="bg-muted/50 h-12 touch-target-lg">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="mp4">
                            <div className="flex items-center gap-2 py-1">
                              <Video className="w-4 h-4 text-blue-500" />
                              MP4 Video
                            </div>
                          </SelectItem>
                          <SelectItem value="mp3">
                            <div className="flex items-center gap-2 py-1">
                              <Music className="w-4 h-4 text-purple-500" />
                              MP3 Audio
                            </div>
                          </SelectItem>
                          <SelectItem value="webm">
                            <div className="flex items-center gap-2 py-1">
                              <FileVideo className="w-4 h-4 text-green-500" />
                              WebM Video
                            </div>
                          </SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                    <div>
                      <label className="text-sm font-medium mb-2 block">Quality</label>
                      <Select value={quality} onValueChange={(v) => v && setQuality(v)}>
                        <SelectTrigger className="bg-muted/50 h-12 touch-target-lg">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {availableFormats.length > 0 ? (
                            availableFormats.map((f) => (
                              <SelectItem key={`${f.format}-${f.quality}`} value={f.quality}>
                                <div className="flex items-center gap-2 py-1">
                                  <Hd className="w-4 h-4" />
                                  {f.label}
                                </div>
                              </SelectItem>
                            ))
                          ) : (
                            <SelectItem value="best">Best Available</SelectItem>
                          )}
                        </SelectContent>
                      </Select>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>

              <AnimatePresence>
                {videoInfo && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={{ opacity: 0, height: 0 }}
                  >
                    <Button
                      onClick={handleDownload}
                      disabled={downloadState.status === "downloading" || downloadState.status === "completed"}
                      className="w-full h-14 sm:h-16 text-base sm:text-lg font-medium bg-gradient-to-r from-blue-600 to-indigo-600 hover:from-blue-700 hover:to-indigo-700 text-white touch-target-lg"
                    >
                      {downloadState.status === "downloading" ? (
                        <div className="flex items-center gap-3">
                          <Loader2 className="w-5 h-5 animate-spin" />
                          <span>Downloading... {Math.round(downloadState.progress)}%</span>
                        </div>
                      ) : downloadState.status === "completed" ? (
                        <div className="flex items-center gap-3">
                          <Check className="w-5 h-5" />
                          <span>Download Complete!</span>
                        </div>
                      ) : (
                        <>
                          <Download className="w-5 h-5 mr-2" />
                          Download {format.toUpperCase()}
                        </>
                      )}
                    </Button>
                  </motion.div>
                )}
              </AnimatePresence>

              <AnimatePresence>
                {downloadState.status === "downloading" && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={{ opacity: 0, height: 0 }}
                    className="mt-3 sm:mt-4"
                  >
                    <div className="h-3 sm:h-2 bg-slate-200 dark:bg-slate-700 rounded-full overflow-hidden">
                      <motion.div
                        className="h-full bg-gradient-to-r from-blue-500 to-indigo-500"
                        initial={{ width: 0 }}
                        animate={{ width: `${Math.min(downloadState.progress, 100)}%` }}
                        transition={{ duration: 0.3 }}
                      />
                    </div>
                    <p className="text-xs text-muted-foreground mt-2 text-center">
                      Processing your download... Please don&apos;t close this tab
                    </p>
                  </motion.div>
                )}
              </AnimatePresence>

              <AnimatePresence>
                {downloadState.status === "completed" && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={{ opacity: 0, height: 0 }}
                  >
                    <div className="mt-3 sm:mt-4 p-3 sm:p-4 bg-green-50 dark:bg-green-950/30 rounded-lg border border-green-200 dark:border-green-800">
                      <div className="flex items-center gap-3 mb-3">
                        <div className="w-10 h-10 bg-green-100 dark:bg-green-900 rounded-full flex items-center justify-center flex-shrink-0">
                          <Check className="w-5 h-5 text-green-600 dark:text-green-400" />
                        </div>
                        <div className="min-w-0">
                          <p className="font-semibold text-green-800 dark:text-green-400 text-sm sm:text-base">
                            Download Ready!
                          </p>
                          <p className="text-xs sm:text-sm text-green-600 dark:text-green-500 truncate">
                            {downloadState.fileName || "Your file is ready"}
                          </p>
                        </div>
                      </div>
                      <div className="flex flex-col sm:flex-row gap-2">
                        <Button 
                          className="flex-1 bg-green-600 hover:bg-green-700 text-white h-12 touch-target-lg"
                          onClick={() => {
                            if (downloadState.downloadUrl) {
                              window.location.href = downloadState.downloadUrl;
                            }
                          }}
                        >
                          <Download className="w-4 h-4 mr-2" />
                          Save File
                        </Button>
                        <Button variant="outline" onClick={resetForm} className="h-12 touch-target-lg">
                          <RefreshCw className="w-4 h-4 mr-2" />
                          New Download
                        </Button>
                      </div>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </CardContent>
          </Card>
        </motion.div>

        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ delay: 0.5 }}
          className="flex flex-wrap items-center justify-center gap-4 sm:gap-6 mt-6 sm:mt-8 text-xs sm:text-sm text-muted-foreground px-2"
        >
          {[
            { icon: Check, text: "No registration" },
            { icon: Shield, text: "Privacy focused" },
            { icon: Hd, text: "HD quality" },
          ].map((badge) => (
            <div key={badge.text} className="flex items-center gap-1.5 sm:gap-2">
              <badge.icon className="w-3.5 h-3.5 sm:w-4 sm:h-4 text-muted-foreground" />
              {badge.text}
            </div>
          ))}
        </motion.div>

        <AnimatePresence>
          {recentDownloads.length > 0 && (
            <motion.div
              initial={{ opacity: 0, y: 20 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -20 }}
              className="mt-6 sm:mt-8 w-full max-w-2xl px-3 sm:px-0"
            >
              <p className="text-xs sm:text-sm text-muted-foreground mb-2 sm:mb-3 text-center">Recent Downloads</p>
              <div className="flex flex-wrap gap-2 justify-center">
                {recentDownloads.map((title, i) => (
                  <Badge key={i} variant="secondary" className="max-w-[150px] sm:max-w-[200px] truncate text-xs touch-target">
                    <Check className="w-3 h-3 mr-1 text-green-500" />
                    {title}
                  </Badge>
                ))}
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </section>

      <section id="features" className="py-12 sm:py-20 px-3 sm:px-4 bg-muted/30">
        <div className="container mx-auto max-w-6xl">
          <motion.div
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            className="text-center mb-10 sm:mb-16"
          >
            <h2 className="text-2xl sm:text-3xl md:text-4xl font-semibold mb-3 sm:mb-4">Features</h2>
            <p className="text-muted-foreground text-base sm:text-lg max-w-2xl mx-auto px-2">
              Simple tools for downloading YouTube content
            </p>
          </motion.div>

          <motion.div
            variants={staggerContainer}
            initial="initial"
            whileInView="animate"
            viewport={{ once: true }}
            className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 sm:gap-6"
          >
            {features.map((feature, index) => (
              <motion.div key={index} variants={fadeInUp}>
                <Card className="h-full border shadow-sm hover:shadow-md transition-shadow bg-gradient-to-b from-white to-slate-50/50 dark:from-slate-900 dark:to-slate-900/50">
                  <CardHeader className="pb-3 sm:pb-4">
                    <div className="w-10 h-10 sm:w-12 sm:h-12 bg-gradient-to-br from-blue-100 to-indigo-100 dark:from-blue-950/30 dark:to-indigo-950/30 rounded-lg flex items-center justify-center mb-3 sm:mb-4">
                      <feature.icon className="w-5 h-5 sm:w-6 sm:h-6 text-blue-600 dark:text-blue-400" />
                    </div>
                    <CardTitle className="text-base sm:text-lg">{feature.title}</CardTitle>
                  </CardHeader>
                  <CardContent>
                    <p className="text-muted-foreground text-sm">{feature.desc}</p>
                  </CardContent>
                </Card>
              </motion.div>
            ))}
          </motion.div>
        </div>
      </section>

      <section className="py-12 sm:py-20 px-3 sm:px-4">
        <div className="container mx-auto max-w-4xl">
          <motion.div
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            className="text-center mb-10 sm:mb-16"
          >
            <h2 className="text-2xl sm:text-3xl md:text-4xl font-semibold mb-3 sm:mb-4">How It Works</h2>
            <p className="text-muted-foreground text-base sm:text-lg px-2">
              Download your favorite videos in 3 simple steps
            </p>
          </motion.div>

          <motion.div
            variants={staggerContainer}
            initial="initial"
            whileInView="animate"
            viewport={{ once: true }}
            className="grid grid-cols-1 sm:grid-cols-3 gap-6 sm:gap-8"
          >
            {[
              { step: "1", title: "Copy URL", desc: "Copy the YouTube video link from your browser" },
              { step: "2", title: "Paste & Analyze", desc: "Paste the URL and click the analyze button" },
              { step: "3", title: "Download", desc: "Choose format and quality, then download" },
            ].map((item, index) => (
              <motion.div key={index} variants={fadeInUp} className="text-center">
                <div className="w-14 h-14 sm:w-16 sm:h-16 bg-gradient-to-br from-blue-600 to-indigo-600 text-white rounded-lg flex items-center justify-center text-xl sm:text-2xl font-semibold mx-auto mb-4 shadow-lg shadow-blue-500/20">
                  {item.step}
                </div>
                <h3 className="font-semibold text-base sm:text-lg mb-2">{item.title}</h3>
                <p className="text-muted-foreground text-sm px-2">{item.desc}</p>
              </motion.div>
            ))}
          </motion.div>
        </div>
      </section>

      <section id="faq" className="py-12 sm:py-20 px-3 sm:px-4 bg-muted/30">
        <div className="container mx-auto max-w-3xl">
          <motion.div
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            className="text-center mb-10 sm:mb-16"
          >
            <h2 className="text-2xl sm:text-3xl md:text-4xl font-semibold mb-3 sm:mb-4">Frequently Asked Questions</h2>
            <p className="text-muted-foreground text-base sm:text-lg px-2">
              Everything you need to know about Athena
            </p>
          </motion.div>

          <motion.div
            variants={staggerContainer}
            initial="initial"
            whileInView="animate"
            viewport={{ once: true }}
            className="space-y-3 sm:space-y-4"
          >
            {[
              {
                q: "Is Athena free to use?",
                a: "Yes, Athena is completely free to use with no hidden charges, subscriptions, or download limits.",
              },
              {
                q: "What formats are supported?",
                a: "Athena supports MP4, WebM for videos and MP3 for audio extraction. Quality options range from 360p to 4K depending on the source video.",
              },
              {
                q: "Is it safe and legal?",
                a: "Athena is safe to use with no malware or tracking. Please respect copyright laws and only download content you have permission to save.",
              },
              {
                q: "How fast are downloads?",
                a: "Download speed depends on your internet connection and the video size. Athena uses optimized servers for the fastest possible downloads.",
              },
              {
                q: "Can I download playlists?",
                a: "Playlist download support is coming soon! For now, you can download individual videos one at a time.",
              },
            ].map((faq, index) => (
              <motion.div key={index} variants={fadeInUp}>
                <Card className="border shadow-sm hover:shadow-md transition-shadow">
                  <CardHeader className="pb-2 sm:pb-3">
                    <CardTitle className="text-sm sm:text-base">{faq.q}</CardTitle>
                  </CardHeader>
                  <CardContent>
                    <p className="text-muted-foreground text-sm">{faq.a}</p>
                  </CardContent>
                </Card>
              </motion.div>
            ))}
          </motion.div>
        </div>
      </section>

      <footer className="border-t py-8 sm:py-12 px-3 sm:px-4 bg-background safe-area-inset-bottom">
        <div className="container mx-auto max-w-6xl">
          <div className="flex flex-col items-center justify-between gap-4 sm:gap-6">
            <div className="flex items-center gap-2">
              <div className="w-7 h-7 sm:w-8 sm:h-8 bg-slate-900 dark:bg-slate-100 rounded-md flex items-center justify-center">
                <YoutubeIcon className="w-4 h-4 sm:w-5 sm:h-5 text-white dark:text-slate-900" />
              </div>
              <span className="font-semibold text-base sm:text-lg">Athena</span>
            </div>
            <p className="text-xs sm:text-sm text-muted-foreground text-center">
              © 2026 Athena. Free YouTube video downloader.
            </p>
            <div className="flex items-center gap-4 sm:gap-6">
              <a href="#" className="text-xs sm:text-sm text-muted-foreground hover:text-foreground transition-colors">
                Privacy
              </a>
              <a href="#" className="text-xs sm:text-sm text-muted-foreground hover:text-foreground transition-colors">
                Terms
              </a>
              <a
                href="https://github.com/Grinsefein/athena"
                target="_blank"
                rel="noopener noreferrer"
                className="text-xs sm:text-sm text-muted-foreground hover:text-foreground transition-colors flex items-center gap-1"
              >
                <GithubIcon className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
                GitHub
              </a>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}
