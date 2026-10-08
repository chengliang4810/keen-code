export type InstructionFile = {
  path: string;
  content: string;
  exists: boolean;
};

export type AgentInstructions = {
  global: InstructionFile;
  project: InstructionFile | null;
};

export type InstructionsWatch = {
  path: string;
  directories: string[];
};
