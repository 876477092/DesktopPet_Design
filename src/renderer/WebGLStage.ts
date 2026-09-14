/**
 * WebGL 渲染舞台：WebGL2 → WebGL1 → Canvas2D 三级探测与合成（`02 §5 K-14`）。
 *
 * 职责（S2-M1）：
 *   1. **三级探测**：依 K-14 顺序取上下文，失败自动降级；同一画布只能持有一个
 *      上下文（HTML 规范），故探测按序取用、命中即停；
 *   2. **子矩形绘制**：`drawSubRect` = 取帧 → 图集子矩形 → 镜像/alpha（K-4 帧绘制
 *      三要素；旋转留待骨骼/物理模块）；
 *   3. **DPI 重建**：`resize` 重设位图尺寸并同步 GL 视口（DPI 变更后重建无错位）。
 *
 * 降级验证手段：三级选择逻辑抽为纯函数 [`pickBackendKind`]（探针注入），可在
 * runner 就绪后以「WebGL 全部失败 → canvas2d」用例直测（本仓库暂无 vitest，见台账）。
 *
 * 边界：不实现骨骼路径（S9-M1 起 `SkeletonRenderer`）、不含气泡/粒子 DOM 层
 * （`LayerHost` 只负责顺序编排）。
 */

/** 图集帧子矩形（物理像素）。 */
export interface FrameSubRect {
  /** 子矩形左上 X（相对图集左上）。 */
  readonly sx: number;
  /** 子矩形左上 Y。 */
  readonly sy: number;
  /** 宽。 */
  readonly sw: number;
  /** 高。 */
  readonly sh: number;
}

/** 单次绘制的选项。 */
export interface DrawOptions {
  /**
   * 是否先清画布（默认 `true`）。交叉淡入双绘（S2-M2）时首绘 `true`、
   * 叠绘 `false`——两帧在同一画布上叠加混合。
   */
  readonly clear?: boolean;
}

/** 舞台后端类型（K-14 三级）。 */
export type StageBackendKind = 'webgl2' | 'webgl1' | 'canvas2d';

/** 上下文探针类型（便于纯函数注入测试）。 */
type ContextProbe = (type: 'webgl2' | 'webgl' | '2d') => RenderingContext | null;

/**
 * 三级探测纯函数（K-14：WebGL2 → WebGL1 → Canvas2D）。
 *
 * 探针返回非空即命中并停止（避免在同一画布上创建冲突上下文）；
 * 全部失败仍返回 `canvas2d` 兜底（真实获取失败时绘制层以空操作降级，不崩溃）。
 */
export function pickBackendKind(probe: ContextProbe): StageBackendKind {
  if (probe('webgl2') !== null) {
    return 'webgl2';
  }
  if (probe('webgl') !== null) {
    return 'webgl1';
  }
  return 'canvas2d';
}

/** 顶点着色器（GLSL ES 1.00，WebGL1/2 通用）：全屏四边形 + 镜像翻转 + UV 映射。 */
const VERT_SRC = `
attribute vec2 aPos;
uniform float uFlipX;
uniform vec4 uUvRect;
varying vec2 vUv;
void main() {
  vec2 p = vec2(aPos.x * uFlipX, aPos.y);
  gl_Position = vec4(p, 0.0, 1.0);
  vec2 uv = vec2(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
  vUv = vec2(uUvRect.x + uv.x * uUvRect.z, uUvRect.y + uv.y * uUvRect.w);
}
`;

/** 片元着色器：纹理采样 × 整体 alpha（预乘 alpha 口径，与透明画布合成对齐）。 */
const FRAG_SRC = `
precision mediump float;
uniform sampler2D uTex;
uniform float uAlpha;
varying vec2 vUv;
void main() {
  vec4 c = texture2D(uTex, vUv);
  gl_FragColor = vec4(c.rgb * uAlpha, c.a * uAlpha);
}
`;

/** 全屏四边形顶点（两个三角形，strip-free）。 */
const QUAD = new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]);

/** GL 资源集合（编译失败 → `null`，绘制降级为告警跳过）。 */
interface GlProgram {
  readonly gl: WebGLRenderingContext;
  readonly program: WebGLProgram;
  readonly buffer: WebGLBuffer;
  readonly aPos: number;
  readonly uFlipX: WebGLUniformLocation | null;
  readonly uAlpha: WebGLUniformLocation | null;
  readonly uUvRect: WebGLUniformLocation | null;
  readonly uTex: WebGLUniformLocation | null;
}

/**
 * 编译着色器并装配四边形；任一步失败返回 `null`（降级不崩溃，`02 §7.4.2`）。
 */
function buildProgram(gl: WebGLRenderingContext): GlProgram | null {
  const compile = (type: number, src: string): WebGLShader | null => {
    const shader = gl.createShader(type);
    if (shader === null) {
      return null;
    }
    gl.shaderSource(shader, src);
    gl.compileShader(shader);
    if (gl.getShaderParameter(shader, gl.COMPILE_STATUS) !== true) {
      console.warn('[WebGLStage] 着色器编译失败：', gl.getShaderInfoLog(shader));
      gl.deleteShader(shader);
      return null;
    }
    return shader;
  };

  const vert = compile(gl.VERTEX_SHADER, VERT_SRC);
  if (vert === null) {
    return null;
  }
  const frag = compile(gl.FRAGMENT_SHADER, FRAG_SRC);
  if (frag === null) {
    gl.deleteShader(vert);
    return null;
  }

  const program = gl.createProgram();
  if (program === null) {
    return null;
  }
  gl.attachShader(program, vert);
  gl.attachShader(program, frag);
  gl.linkProgram(program);
  gl.deleteShader(vert);
  gl.deleteShader(frag);
  if (gl.getProgramParameter(program, gl.LINK_STATUS) !== true) {
    console.warn('[WebGLStage] 程序链接失败：', gl.getProgramInfoLog(program));
    return null;
  }

  const buffer = gl.createBuffer();
  if (buffer === null) {
    return null;
  }
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, QUAD, gl.STATIC_DRAW);

  const aPos = gl.getAttribLocation(program, 'aPos');
  if (aPos < 0) {
    return null;
  }

  const uni = (name: string): WebGLUniformLocation | null =>
    gl.getUniformLocation(program, name);

  return {
    gl,
    program,
    buffer,
    aPos,
    uFlipX: uni('uFlipX'),
    uAlpha: uni('uAlpha'),
    uUvRect: uni('uUvRect'),
    uTex: uni('uTex'),
  };
}

/**
 * WebGL 渲染舞台（持有画布唯一上下文与 GL 资源）。
 */
export class WebGLStage {
  private readonly canvas: HTMLCanvasElement;
  private readonly kind: StageBackendKind;
  private readonly ctx2d: CanvasRenderingContext2D | null;
  private readonly gfx: GlProgram | null;
  private texture: WebGLTexture | null = null;
  private textureSource = '';
  private glWarned = false;

  private constructor(
    canvas: HTMLCanvasElement,
    kind: StageBackendKind,
    ctx2d: CanvasRenderingContext2D | null,
    gfx: GlProgram | null,
  ) {
    this.canvas = canvas;
    this.kind = kind;
    this.ctx2d = ctx2d;
    this.gfx = gfx;
  }

  /** 当前后端类型（K-14 三级之一）。 */
  get backend(): StageBackendKind {
    return this.kind;
  }

  /**
   * 三级探测创建舞台（K-14：WebGL2 → WebGL1 → Canvas2D，失败自动降级）。
   */
  static create(canvas: HTMLCanvasElement): WebGLStage {
    const probe: ContextProbe = (type) => {
      if (type === '2d') {
        return canvas.getContext('2d');
      }
      if (type === 'webgl') {
        return canvas.getContext('webgl');
      }
      return canvas.getContext('webgl2');
    };
    const kind = pickBackendKind(probe);

    if (kind === 'webgl2' || kind === 'webgl1') {
      const contextType = kind === 'webgl2' ? 'webgl2' : 'webgl';
      // 探测阶段已确认该类型可获取；此处重取同一上下文（单上下文规则下幂等）。
      const gl = canvas.getContext(contextType) as WebGLRenderingContext | null;
      if (gl !== null) {
        const gfx = buildProgram(gl);
        if (gfx !== null) {
          // 预乘 alpha 口径：从 ImageBitmap 上传时同步预乘（透明画布合成一致）。
          gfx.gl.pixelStorei(gfx.gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, 1);
          // 帧内混合：交叉淡入双绘（S2-M2）第二笔不清画布、按预乘 alpha
          // over 合成叠在第一笔上（单帧绘制时缓冲已清透明，结果等价替换）。
          gfx.gl.enable(gfx.gl.BLEND);
          gfx.gl.blendFunc(gfx.gl.ONE, gfx.gl.ONE_MINUS_SRC_ALPHA);
          const texture = gfx.gl.createTexture();
          return new WebGLStage(canvas, kind, null, gfx).withTexture(texture);
        }
        // GL 上下文在但着色器装配失败：无法再取 2D（单上下文规则），降级为空绘制。
        console.warn('[WebGLStage] GL 程序装配失败，绘制将降级为跳帧');
      }
    }

    const ctx2d = canvas.getContext('2d');
    if (kind !== 'canvas2d' && ctx2d === null) {
      // 理论不可达（探测已确认），防御性告警。
      console.warn('[WebGLStage] 画布 2D 上下文获取失败，绘制将降级为跳帧');
    }
    return new WebGLStage(canvas, ctx2d === null ? kind : 'canvas2d', ctx2d, null);
  }

  /** 附加 GL 纹理对象（私有构造的流式 setter）。 */
  private withTexture(texture: WebGLTexture | null): WebGLStage {
    this.texture = texture;
    return this;
  }

  /**
   * 重设画布物理位图尺寸（DPI / resize 重建路径）。
   *
   * 同步 GL 视口；画布位图被重置为透明，需由调用方重绘最后一帧（`LayerHost.render`）。
   */
  resize(physicalW: number, physicalH: number): void {
    if (this.canvas.width === physicalW && this.canvas.height === physicalH) {
      return;
    }
    this.canvas.width = physicalW;
    this.canvas.height = physicalH;
    if (this.gfx !== null) {
      this.gfx.gl.viewport(0, 0, physicalW, physicalH);
    }
  }

  /**
   * 绘制一帧：图集位图子矩形 → 画布（镜像可选、整体 alpha）。
   *
   * 目标矩形恒为**整幅画布**（宠物窗口 256×256 与单帧锚点「底部中心」契约一致，
   * `02 §4.4`）。位图不可用 / 子矩形非法由调用方（`FrameRenderer`）拦截；
   * GL 路径装配失败时告警跳过（不崩溃降级）。
   *
   * @param bitmap    已解码图集（ImageBitmap）
   * @param atlasName 图集文件名（GL 纹理复用键；内容变化即重传）
   * @param rect      帧子矩形（图集内，物理像素）
   * @param mirror    水平镜像（K-4）
   * @param alpha     整体不透明度（0.0~1.0）
   * @param opts      绘制选项（交叉淡入双绘时叠绘 `clear: false`，S2-M2）
   */
  drawSubRect(
    bitmap: ImageBitmap,
    atlasName: string,
    rect: FrameSubRect,
    mirror: boolean,
    alpha: number,
    opts: DrawOptions = {},
  ): void {
    const clear = opts.clear !== false;
    if (this.gfx !== null) {
      this.drawGl(bitmap, atlasName, rect, mirror, alpha, clear);
      return;
    }
    if (this.ctx2d !== null) {
      this.draw2d(bitmap, rect, mirror, alpha, clear);
      return;
    }
    this.warnOnce('[WebGLStage] 无可用绘制上下文，跳帧');
  }

  /** GL 绘制路径（WebGL1/2 共用同一着色器与纹理管线）。 */
  private drawGl(
    bitmap: ImageBitmap,
    atlasName: string,
    rect: FrameSubRect,
    mirror: boolean,
    alpha: number,
    clear: boolean,
  ): void {
    const gfx = this.gfx;
    if (gfx === null || this.texture === null) {
      this.warnOnce('[WebGLStage] GL 资源未就绪，跳帧');
      return;
    }
    const gl = gfx.gl;
    if (this.textureSource !== atlasName) {
      this.uploadTexture(gl, bitmap);
      this.textureSource = atlasName;
    }

    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    if (clear) {
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
    }

    gl.useProgram(gfx.program);
    gl.bindBuffer(gl.ARRAY_BUFFER, gfx.buffer);
    gl.enableVertexAttribArray(gfx.aPos);
    gl.vertexAttribPointer(gfx.aPos, 2, gl.FLOAT, false, 0, 0);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    if (gfx.uTex !== null) {
      gl.uniform1i(gfx.uTex, 0);
    }
    if (gfx.uFlipX !== null) {
      gl.uniform1f(gfx.uFlipX, mirror ? -1 : 1);
    }
    if (gfx.uAlpha !== null) {
      gl.uniform1f(gfx.uAlpha, alpha);
    }
    if (gfx.uUvRect !== null) {
      const aw = bitmap.width;
      const ah = bitmap.height;
      gl.uniform4f(gfx.uUvRect, rect.sx / aw, rect.sy / ah, rect.sw / aw, rect.sh / ah);
    }
    gl.drawArrays(gl.TRIANGLES, 0, 6);
  }

  /** 上传图集位图为纹理（CLAMP_TO_EDGE + LINEAR，NPOT 图集安全）。 */
  private uploadTexture(gl: WebGLRenderingContext, bitmap: ImageBitmap): void {
    if (this.texture === null) {
      return;
    }
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bitmap);
  }

  /** Canvas2D 绘制路径（三级探测兜底，K-14）。 */
  private draw2d(
    bitmap: ImageBitmap,
    rect: FrameSubRect,
    mirror: boolean,
    alpha: number,
    clear: boolean,
  ): void {
    const ctx = this.ctx2d;
    if (ctx === null) {
      this.warnOnce('[WebGLStage] 无可用绘制上下文，跳帧');
      return;
    }
    const w = this.canvas.width;
    const h = this.canvas.height;
    if (clear) {
      ctx.clearRect(0, 0, w, h);
    }
    ctx.save();
    ctx.globalAlpha = alpha;
    if (mirror) {
      ctx.translate(w, 0);
      ctx.scale(-1, 1);
    }
    ctx.drawImage(bitmap, rect.sx, rect.sy, rect.sw, rect.sh, 0, 0, w, h);
    ctx.restore();
  }

  /** 告警去重（避免每帧刷屏）。 */
  private warnOnce(message: string): void {
    if (!this.glWarned) {
      this.glWarned = true;
      console.warn(message);
    }
  }
}
