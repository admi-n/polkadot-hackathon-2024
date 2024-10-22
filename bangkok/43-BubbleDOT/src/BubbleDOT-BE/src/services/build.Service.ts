import { exec } from 'child_process';
import { promises as fs } from 'fs';
import path from 'path';


const findCargoTomlDir = async (startDir: string): Promise<string | null> => {
  let currentDir = startDir;
  while (true) {
    const cargoTomlPath = path.join(currentDir, 'Cargo.toml');
    console.log(`Checking Cargo.toml in: ${cargoTomlPath}`);
    try {
      await fs.access(cargoTomlPath);
      return currentDir;
    } catch (err) {
      const parentDir = path.dirname(currentDir);
      console.log(`Parent directory: ${parentDir}`);
      if (parentDir === currentDir) {
        return null;
      }
      currentDir = parentDir;
    }
  }
};

const buildProject = async (): Promise<void> => {
  const bucketName = process.env.BUCKET_NAME;
  if (!bucketName) {
    throw new Error('BUCKET_NAME is not defined');
  }

  const startDir = path.join(process.cwd(), 'Downloads', bucketName, 'home', 'project');
  console.log(`Starting search for Cargo.toml from: ${startDir}`);

  const cargoDir = await findCargoTomlDir(startDir);
  if (!cargoDir) {
    throw new Error('Cargo.toml not found in any parent directory');
  }
  return new Promise((resolve, reject) => {
    console.log(`Starting cargo build in: ${cargoDir}`);
    exec('cargo build', { cwd: cargoDir }, (error, stdout, stderr) => {
      if (error) {
        console.error(`Error executing cargo build: ${error.message}`);
        return reject(error);

      }
      if (stdout) {
        console.log(`Build stdout: ${stdout}`);
      }

      if (stderr) {
        console.warn(`Build stderr: ${stderr}`);
      }
      resolve();
    });
  });
};


export { buildProject };