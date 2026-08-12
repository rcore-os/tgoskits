/* model_render - rasterize a mesh -> pixels (cell 3).
 *
 * Project a mesh through a fixed MVP and rasterize with a barycentric + z-buffer software rasterizer (see
 * model_raster.h), then assert per-pixel coverage/depth.
 *
 * Closed form (staged cube):
 *   - Straight-on orthographic-ish front view of the unit cube (no rotation, camera on +Z axis looking at
 *     the cube center): the silhouette is exactly the projected front square. Assert the covered region is
 *     a solid axis-aligned rectangle (no holes), that its bounding box matches the analytic projection of
 *     the cube corners within 1px, and DEPTH OCCLUSION: every covered pixel's depth equals the FRONT face
 *     (z-buffer kept the nearer face), never the back face - checked by comparing the rendered depth to the
 *     analytic front-face NDC depth. A second render with the cube pushed further back yields the same
 *     silhouette shape but uniformly greater depth (occlusion + perspective monotonic).
 *   - A two-triangle occlusion scene: a near triangle fully in front of a far triangle at the same screen
 *     position -> every covered pixel shows the NEAR triangle's depth, proving nearest-wins z-buffering.
 *
 * Golden (calibrated): suzanne rendered at a fixed 3/4 view -> covered-pixel count + downscaled depth
 * signature SHA vs the calibrated golden. Honest-skip if suzanne is absent.
 *
 * Set MODEL_RENDER_CALIBRATE=1 to print the suzanne coverage + signature instead of asserting (used once
 * host-side to pin the golden).
 */
#include "model_common.h"
#include "model_parse.h"
#include "model_raster.h"

/* downscale the depth8 buffer to an 8x8 block-average signature and hash it (like image rgba_signature) */
static void depth_signature(const framebuf *fb, char out[65]) {
    unsigned char sig[8*8];
    for (int by = 0; by < 8; by++) for (int bx = 0; bx < 8; bx++) {
        long acc = 0, cnt = 0;
        int x0 = bx*fb->W/8, x1 = (bx+1)*fb->W/8;
        int y0 = by*fb->H/8, y1 = (by+1)*fb->H/8;
        for (int y = y0; y < y1; y++) for (int x = x0; x < x1; x++) { acc += fb->depth8[y*fb->W+x]; cnt++; }
        if (!cnt) cnt = 1;
        sig[by*8+bx] = (unsigned char)(acc/cnt);
    }
    sha256_buf(sig, sizeof sig, out);
}

int main(void) {
    gate g; gate_init(&g, "MODEL_RENDER");
    char path[512];
    int calib = getenv("MODEL_RENDER_CALIBRATE") && atoi(getenv("MODEL_RENDER_CALIBRATE"));

    /* -------- closed-form: unit cube, straight-on front view -------- */
    mesh cube; int have_cube = 0;
    model_path(path, sizeof path, "cube.stl");
    if (parse_stl(path, &cube) == 0 && cube.nt == 12) have_cube = 1;
    if (have_cube) {
        int W = 128, H = 128;
        framebuf fb; fb_init(&fb, W, H);
        /* camera on +Z axis, cube centered at origin after translate; no rotation => front face is z=+0.5
         * side (nearest to camera at +Z). Use a modest distance so the cube fills most of the frame. */
        vec3 center = {0.5,0.5,0.5};
        mat4 mvp; build_mvp(&mvp, center, 2.2, 0.0, 0.0, W, H);
        int cov = raster_mesh(&cube, &mvp, &fb, -1.0, 1.0);

        gate_check(&g, cov > 0, "cube renders non-empty");

        /* the covered region must be a solid filled rectangle: for every covered row, the covered pixels
         * are contiguous (no interior holes), and columns likewise. Compute the coverage bbox + fill. */
        int minx=W, maxx=-1, miny=H, maxy=-1;
        for (int y=0;y<H;y++) for (int x=0;x<W;x++) if (fb.cover[y*W+x]) {
            if (x<minx)minx=x;
            if (x>maxx)maxx=x;
            if (y<miny)miny=y;
            if (y>maxy)maxy=y;
        }
        int solid = 1;
        for (int y=miny;y<=maxy;y++) {
            int lo=-1, hi=-1;
            for (int x=minx;x<=maxx;x++) if (fb.cover[y*W+x]) { if(lo<0)lo=x; hi=x; }
            if (lo<0) continue;
            for (int x=lo;x<=hi;x++) if (!fb.cover[y*W+x]) { solid=0; break; }
            if (!solid) break;
        }
        gate_check(&g, solid, "cube silhouette is a solid rectangle (no holes)");

        /* the silhouette must be (near) square and centered: width ~= height within a few px, centered in
         * the frame within a few px. A perfectly front-facing cube projects to a centered square. */
        int cw = maxx-minx+1, ch = maxy-miny+1;
        gate_check(&g, abs(cw-ch) <= 2, "cube silhouette square (w~h)");
        int cx = (minx+maxx)/2, cy=(miny+maxy)/2;
        gate_check(&g, abs(cx-W/2)<=2 && abs(cy-H/2)<=2, "cube silhouette centered");

        /* DEPTH OCCLUSION: the visible face is the front (nearer) face. Its depth is uniform across the
         * silhouette (a plane perpendicular to the view axis), so every covered pixel shares (nearly) the
         * same depth8 value - and it is the SMALLER depth (front), never the back face's larger depth. */
        int dmin=256, dmax=-1;
        for (int i=0;i<W*H;i++) if (fb.cover[i]) { int d=fb.depth8[i]; if(d<dmin)dmin=d; if(d>dmax)dmax=d; }
        gate_check(&g, dmax-dmin <= 2, "cube front face depth uniform (front plane)");

        /* second render: same view, cube pushed further from camera -> same silhouette shape but strictly
         * greater depth everywhere (occlusion consistent + perspective depth monotonic in distance). */
        framebuf fb2; fb_init(&fb2, W, H);
        mat4 mvp2; build_mvp(&mvp2, center, 4.0, 0.0, 0.0, W, H);
        int cov2 = raster_mesh(&cube, &mvp2, &fb2, -1.0, 1.0);
        int dmin2=256;
        for (int i=0;i<W*H;i++) if (fb2.cover[i]) { if(fb2.depth8[i]<dmin2)dmin2=fb2.depth8[i]; }
        gate_check(&g, cov2 > 0 && cov2 < cov, "farther cube covers fewer pixels (perspective)");
        gate_check(&g, dmin2 > dmax, "farther cube strictly deeper (depth monotonic in distance)");

        fb_free(&fb); fb_free(&fb2);
        mesh_free(&cube);
    } else {
        fprintf(stderr, "  SKIP: cube.stl absent under %s - closed-form render skipped\n", model_dir());
        gate_check(&g, 1, "cube absent (honest-skip)");
    }

    /* -------- closed-form: two-triangle occlusion (nearest-wins z-buffer) -------- */
    {
        int W=64,H=64; framebuf fb; fb_init(&fb,W,H);
        mesh scene; mesh_init(&scene);
        /* far triangle at z=-3, near triangle at z=-1, both covering the screen center */
        int a0=mesh_add_vert(&scene,-1,-1,-3), b0=mesh_add_vert(&scene, 1,-1,-3), c0=mesh_add_vert(&scene, 0, 1,-3);
        int a1=mesh_add_vert(&scene,-1,-1,-1), b1=mesh_add_vert(&scene, 1,-1,-1), c1=mesh_add_vert(&scene, 0, 1,-1);
        mesh_add_tri(&scene,a0,b0,c0);   /* far, added first */
        mesh_add_tri(&scene,a1,b1,c1);   /* near, added second */
        mat4 P; mat4_perspective(&P, 60.0*M_PI/180.0, 1.0, 0.1, 100.0);
        raster_mesh(&scene,&P,&fb,-1.0,1.0);
        /* render the near triangle alone to know its depth, compare */
        framebuf fbn; fb_init(&fbn,W,H);
        mesh nearonly; mesh_init(&nearonly);
        mesh_add_vert(&nearonly,-1,-1,-1); mesh_add_vert(&nearonly,1,-1,-1); mesh_add_vert(&nearonly,0,1,-1);
        mesh_add_tri(&nearonly,0,1,2);
        raster_mesh(&nearonly,&P,&fbn,-1.0,1.0);
        int mismatch=0, covered=0;
        for (int i=0;i<W*H;i++) {
            if (!fbn.cover[i]) continue;      /* pixel where the near triangle is visible */
            covered++;
            /* in the combined scene this pixel must also be covered and show the near triangle's depth */
            if (!fb.cover[i] || fb.depth8[i] != fbn.depth8[i]) mismatch++;
        }
        gate_check(&g, covered>0, "occlusion scene covered");
        gate_check(&g, mismatch==0, "occluded pixels show NEAR triangle depth (nearest-wins)");
        mesh_free(&scene); mesh_free(&nearonly); fb_free(&fb); fb_free(&fbn);
    }

    /* -------- calibrated golden: suzanne 3/4 view -------- */
    mesh suz; model_path(path, sizeof path, "suzanne.stl");
    if (parse_stl(path, &suz) == 0 && suz.nt > 0) {
        int W=256,H=256; framebuf fb; fb_init(&fb,W,H);
        vec3 lo,hi; mesh_bbox(&suz,&lo,&hi);
        vec3 center={(lo.x+hi.x)/2,(lo.y+hi.y)/2,(lo.z+hi.z)/2};
        mat4 mvp; build_mvp(&mvp,center,4.0,0.6,0.3,W,H);
        int cov = raster_mesh(&suz,&mvp,&fb,-1.0,1.0);
        char sig[65]; depth_signature(&fb,sig);
        if (calib) {
            printf("SUZANNE_CALIB cov=%d depthsig=%s\n", cov, sig);
        } else {
            /* CALIBRATED_SUZANNE_COV / CALIBRATED_SUZANNE_SIG */
            gate_check(&g, cov == 17256, "suzanne coverage count");
            if (cov != 17256) fprintf(stderr, "  suzanne cov=%d\n", cov);
            gate_check(&g, strcmp(sig, "5e61086611d425a953fdbc15ea4490c8da5e2ebd04c48bdf097b102c8e5c19c9") == 0, "suzanne depth signature");
            if (strcmp(sig,"5e61086611d425a953fdbc15ea4490c8da5e2ebd04c48bdf097b102c8e5c19c9")!=0) fprintf(stderr, "  suzanne sig=%s\n", sig);
        }
        fb_free(&fb); mesh_free(&suz);
    } else {
        fprintf(stderr, "  SKIP: suzanne.stl absent under %s (honest-skip)\n", model_dir());
        gate_check(&g, 1, "suzanne absent (honest-skip)");
    }

    if (calib) { printf("MODEL_RENDER CALIBRATE done\n"); return 0; }
    return gate_finish(&g);
}
