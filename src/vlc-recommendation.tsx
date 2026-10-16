import { invoke } from "@tauri-apps/api/core";
import { Button, Icon } from "./ui";

export function VlcRecommendation({ report }: { report: (error: unknown) => void }) {
  return (
    <aside className="vlc-recommendation" aria-label="Recommended player">
      <Icon name="play_circle_outline" />
      <div>
        <strong>More formats. Better playback.</strong>
        <p>Install VLC 3 in Applications for broader video, audio and subtitle support.
          The built-in player is available in the meantime.</p>
      </div>
      <Button icon="open_in_new" onClick={() => void invoke("open_vlc_download").catch(report)}>
        Get VLC
      </Button>
    </aside>
  );
}
