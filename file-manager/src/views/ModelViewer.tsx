import { LoaderCircle } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import {
  AmbientLight,
  Box3,
  BufferGeometry,
  Color,
  DirectionalLight,
  DoubleSide,
  Group,
  HemisphereLight,
  Material,
  Mesh,
  MeshPhongMaterial,
  Object3D,
  PerspectiveCamera,
  Scene,
  Sphere,
  Texture,
  Vector3,
  WebGLRenderer,
} from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import { ThreeMFLoader } from "three/examples/jsm/loaders/3MFLoader.js";
import { OBJLoader } from "three/examples/jsm/loaders/OBJLoader.js";
import { STLLoader } from "three/examples/jsm/loaders/STLLoader.js";
import { fileUrl } from "../lib/api";
import { t } from "../i18n";

// Loaded on demand (`React.lazy` in Preview.tsx): three.js stays out of the
// window's first bundle.

/** The accent's middle color, for models that bring no colors of their own. */
function accentColor(): Color {
  const css = getComputedStyle(document.documentElement).getPropertyValue("--a2").trim();
  try {
    return new Color().setStyle(css || "#c46cf2");
  } catch {
    return new Color("#c46cf2");
  }
}

const accentMaterial = (vertexColors = false) =>
  new MeshPhongMaterial({ color: vertexColors ? 0xffffff : accentColor(), vertexColors, shininess: 28, specular: 0x262626, side: DoubleSide });

/** A 3MF part without materials of its own gets the loader's plain white Phong: drawn in the accent instead. */
function isPlainWhite(m: Material): boolean {
  return m instanceof MeshPhongMaterial && !m.map && !m.vertexColors && m.color.getHex() === 0xffffff;
}

/** Normals the file left out, or wrote as zeros (many STL exporters do: the shading came out
 * black), computed from the faces. */
function ensureNormals(g: BufferGeometry) {
  const n = g.getAttribute("normal");
  if (n) {
    const values = n.array;
    for (let i = 0; i < values.length; i++) if (values[i] !== 0) return;
  }
  g.computeVertexNormals();
}

/** The model as a scene object, upright (STL and 3MF are Z-up, as printers are; OBJ is Y-up). */
function parse(ext: string, bytes: ArrayBuffer): Object3D {
  if (ext === "stl") {
    const geometry = new STLLoader().parse(bytes);
    ensureNormals(geometry);
    const colored = (geometry as BufferGeometry & { hasColors?: boolean }).hasColors === true;
    const mesh = new Mesh(geometry, accentMaterial(colored));
    mesh.rotation.x = -Math.PI / 2;
    return mesh;
  }
  if (ext === "obj") {
    const group = new OBJLoader().parse(new TextDecoder().decode(bytes));
    group.traverse((o) => {
      if (!(o instanceof Mesh)) return;
      const g = o.geometry as BufferGeometry;
      ensureNormals(g);
      const old = o.material as Material | Material[];
      (Array.isArray(old) ? old : [old]).forEach((m) => m.dispose());
      o.material = accentMaterial(Boolean(g.getAttribute("color")));
    });
    return group;
  }
  const group: Group = new ThreeMFLoader().parse(bytes);
  group.traverse((o) => {
    if (!(o instanceof Mesh)) return;
    const mats = Array.isArray(o.material) ? (o.material as Material[]) : [o.material as Material];
    if (mats.every(isPlainWhite)) {
      mats.forEach((m) => m.dispose());
      o.material = accentMaterial();
    } else mats.forEach((m) => (m.side = DoubleSide));
  });
  group.rotation.x = -Math.PI / 2;
  return group;
}

function triangles(root: Object3D): number {
  let n = 0;
  root.traverse((o) => {
    if (!(o instanceof Mesh)) return;
    const g = o.geometry as BufferGeometry;
    n += Math.floor((g.index ? g.index.count : (g.getAttribute("position")?.count ?? 0)) / 3);
  });
  return n;
}

function dispose(root: Object3D) {
  root.traverse((o) => {
    if (!(o instanceof Mesh)) return;
    (o.geometry as BufferGeometry).dispose();
    const mats = Array.isArray(o.material) ? (o.material as Material[]) : [o.material as Material];
    for (const m of mats) {
      for (const v of Object.values(m)) if (v instanceof Texture) v.dispose();
      m.dispose();
    }
  });
}

/**
 * A 3D model (STL, 3MF, OBJ) drawn with three.js over the glass: centered and
 * framed, lit softly, in the accent color unless it brings its own (a 3MF's
 * materials, an STL's or OBJ's vertex colors). Drag turns it, the right button
 * or two fingers pan, the wheel zooms, a double-click goes back to the first
 * view. Drawn only when something changes (no animation loop); the renderer,
 * the geometry and the materials are released when it goes (a page gets few
 * WebGL contexts).
 */
export default function ModelViewer({ path, ext, large, onFail }: { path: string; ext: string; large: boolean; onFail: (why: string) => void }) {
  const box = useRef<HTMLDivElement | null>(null);
  const [state, setState] = useState<{ loading: boolean; count: number }>({ loading: true, count: 0 });
  const fail = useRef(onFail);
  fail.current = onFail;

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    let renderer: WebGLRenderer;
    try {
      renderer = new WebGLRenderer({ antialias: true, alpha: true, powerPreference: "low-power" });
    } catch {
      fail.current(t("views.model.noWebgl"));
      return;
    }
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    renderer.setClearColor(0x000000, 0);
    const canvas = renderer.domElement;
    canvas.className = "block h-full w-full";
    el.appendChild(canvas);

    const scene = new Scene();
    const camera = new PerspectiveCamera(35, 1, 0.01, 1000);
    scene.add(new HemisphereLight(0xffffff, 0x3a3550, 1.6));
    scene.add(new AmbientLight(0xffffff, 0.25));
    // A key light that follows the camera: the side facing the viewer is always lit.
    const key = new DirectionalLight(0xffffff, 1.8);
    key.position.set(0.6, 0.8, 1);
    camera.add(key);
    scene.add(camera);
    const controls = new OrbitControls(camera, canvas);
    controls.zoomSpeed = 0.8;
    let model: Object3D | null = null;
    let alive = true;

    const draw = () => renderer.render(scene, camera);
    const fit = () => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      if (w === 0 || h === 0) return;
      renderer.setSize(w, h, false);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
      draw();
    };
    controls.addEventListener("change", draw);
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    const reset = () => {
      controls.reset();
      draw();
    };
    canvas.addEventListener("dblclick", reset);

    const abort = new AbortController();
    fetch(fileUrl(path), { signal: abort.signal })
      .then((r) => {
        if (!r.ok) throw new Error(String(r.status));
        return r.arrayBuffer();
      })
      .then((bytes) => {
        if (!alive) return;
        const object = parse(ext, bytes);
        // Centered on a pivot, framed from the front, a little to the right and above.
        const pivot = new Group();
        pivot.add(object);
        const bounds = new Box3().setFromObject(object);
        if (bounds.isEmpty()) throw new Error("empty");
        object.position.sub(bounds.getCenter(new Vector3()));
        scene.add(pivot);
        model = pivot;
        const radius = Math.max(bounds.getBoundingSphere(new Sphere()).radius, 1e-6);
        const distance = (radius / Math.sin(((camera.fov / 2) * Math.PI) / 180)) * 1.1;
        camera.near = distance / 100;
        camera.far = distance * 100;
        camera.position.copy(new Vector3(0.9, 0.7, 1.3).normalize().multiplyScalar(distance));
        camera.lookAt(0, 0, 0);
        camera.updateProjectionMatrix();
        controls.target.set(0, 0, 0);
        controls.minDistance = radius * 0.2;
        controls.maxDistance = distance * 8;
        controls.update();
        controls.saveState();
        setState({ loading: false, count: triangles(object) });
        fit();
      })
      .catch((e) => {
        if (alive && (e as Error).name !== "AbortError") fail.current(t("views.model.failed"));
      });

    return () => {
      alive = false;
      abort.abort();
      ro.disconnect();
      canvas.removeEventListener("dblclick", reset);
      controls.dispose();
      if (model) dispose(model);
      renderer.dispose();
      renderer.forceContextLoss();
      canvas.remove();
    };
  }, [path, ext]);

  return (
    <div className={`relative h-full min-h-0 overflow-hidden rounded-xl ${large ? "" : "bg-black/15"}`}>
      <div ref={box} className="absolute inset-0 cursor-grab active:cursor-grabbing" />
      {state.loading ? (
        <div className="pointer-events-none absolute inset-0 grid place-items-center text-dim">
          <LoaderCircle className="spin h-5 w-5" />
        </div>
      ) : (
        <p className={`pointer-events-none absolute inset-x-0 bottom-2 text-center text-dim ${large ? "text-[0.7rem]" : "text-[0.6rem]"}`}>
          {large ? `${t("views.model.triangles", { count: state.count })} · ${t("views.model.hint")}` : t("views.model.triangles", { count: state.count })}
        </p>
      )}
    </div>
  );
}
