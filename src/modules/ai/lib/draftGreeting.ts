const BOUNDARY_HOURS = [5, 9, 12, 14, 18, 23] as const;

export function draftGreeting(date: Date): string {
  const hour = date.getHours();
  if (hour >= 5 && hour < 9) return "Good morning. A new day begins.";
  if (hour >= 9 && hour < 12) return "Good morning. How can I help you?";
  if (hour >= 12 && hour < 14) return "Good afternoon. Time for a break?";
  if (hour >= 14 && hour < 18)
    return "Good afternoon. Leave the next step to me.";
  if (hour >= 18 && hour < 23) return "Good evening. You've worked hard today.";
  return "It's getting late. Remember to take care of yourself.";
}

export function nextDraftGreetingDelay(date: Date): number {
  const next = new Date(date);
  const hour = BOUNDARY_HOURS.find((boundary) => boundary > date.getHours());
  if (hour === undefined) next.setDate(next.getDate() + 1);
  next.setHours(hour ?? BOUNDARY_HOURS[0], 0, 0, 0);
  return Math.max(1, next.getTime() - date.getTime());
}

export function fitDraftGreeting(
  availableWidth: number,
  naturalWidth: number,
  maxFontSize: number,
): number {
  if (
    !Number.isFinite(availableWidth) ||
    !Number.isFinite(naturalWidth) ||
    availableWidth <= 0 ||
    naturalWidth <= 0
  ) {
    return maxFontSize;
  }
  return Math.max(
    Math.max(20, maxFontSize - 10),
    Math.min(
      maxFontSize,
      Math.floor(maxFontSize * (availableWidth / naturalWidth)),
    ),
  );
}
