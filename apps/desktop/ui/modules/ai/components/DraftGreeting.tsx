import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "@/modules/i18n";
import {
  draftGreeting,
  fitDraftGreeting,
  nextDraftGreetingDelay,
} from "@/modules/ai/lib/draftGreeting";

export function DraftGreeting() {
  const tr = useTranslation();
  const [message, setMessage] = useState(() => draftGreeting(new Date()));
  const [fontSize, setFontSize] = useState<number>();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const measurementRef = useRef<HTMLSpanElement>(null);
  const greeting = tr(message);

  useEffect(() => {
    let timeout: number;
    const update = () => {
      const now = new Date();
      setMessage(draftGreeting(now));
      window.clearTimeout(timeout);
      timeout = window.setTimeout(update, nextDraftGreetingDelay(now));
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") update();
    };
    update();
    window.addEventListener("focus", update);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      window.clearTimeout(timeout);
      window.removeEventListener("focus", update);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, []);

  useLayoutEffect(() => {
    const heading = headingRef.current;
    const measurement = measurementRef.current;
    if (!heading || !measurement) return;
    let frame: number | null = null;
    const measure = () => {
      frame = null;
      const style = getComputedStyle(heading);
      const padding =
        Number.parseFloat(style.paddingLeft) +
        Number.parseFloat(style.paddingRight);
      setFontSize(
        fitDraftGreeting(
          heading.clientWidth - padding,
          measurement.getBoundingClientRect().width,
          Number.parseFloat(getComputedStyle(measurement).fontSize),
        ),
      );
    };
    const schedule = () => {
      if (frame === null) frame = window.requestAnimationFrame(measure);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(heading);
    observer.observe(measurement);
    measure();
    return () => {
      observer.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, []);

  return (
    <div className="relative mb-10 w-full shrink-0 sm:mb-8">
      <div
        aria-hidden="true"
        className="pointer-events-none absolute top-1/2 left-1/2 -mt-10 h-80 w-[min(72vw,25rem)] -translate-x-1/2 -translate-y-1/2 select-none [mask-image:linear-gradient(to_bottom,black,transparent_85%)]"
      >
        <img
          src="/logo.png"
          alt=""
          draggable={false}
          className="size-full object-contain opacity-[0.07] dark:opacity-[0.14]"
        />
      </div>
      <h1
        ref={headingRef}
        data-draft-greeting
        style={{ fontSize }}
        className="relative w-full overflow-hidden px-4 text-center text-3xl leading-[1.2] font-medium tracking-tight text-foreground"
      >
        <span
          ref={measurementRef}
          aria-hidden="true"
          className="pointer-events-none invisible absolute text-3xl leading-[1.2] whitespace-nowrap"
        >
          {greeting}
        </span>
        {greeting}
      </h1>
    </div>
  );
}
