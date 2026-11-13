import sys
from PIL import Image, ImageDraw
out = sys.argv[1]
# A sheep, side view facing left, on a blue ground (its body is white).
im=Image.new("RGB",(96,96),(156,196,228)); d=ImageDraw.Draw(im)
dark=(60,60,64); wool=(244,244,244)
for x in (34,48,62): d.rectangle((x,62,x+5,84),fill=dark)
d.rectangle((70,62,75,84),fill=dark)
for (cx,cy,r) in [(38,48,14),(52,42,15),(66,46,14),(74,54,11),(44,58,12),(60,58,13),(30,54,10)]:
    d.ellipse((cx-r,cy-r,cx+r,cy+r),fill=wool)
d.ellipse((10,34,30,56),fill=dark)
d.polygon([(22,36),(32,30),(28,40)],fill=dark)
d.ellipse((15,40,19,44),fill=(250,250,250))
im.save(out+"/sheep.png")
# A wolf, side view facing left, on white.
im=Image.new("RGB",(96,96),(255,255,255)); d=ImageDraw.Draw(im)
grey=(160,160,168); back=(110,110,118)
for x in (28,38,62,72): d.rectangle((x,58,x+5,84),fill=back)
d.ellipse((22,36,82,66),fill=grey)
d.pieslice((22,30,82,60),180,360,fill=back)
d.polygon([(80,44),(94,24),(90,46)],fill=back)
d.polygon([(6,40),(28,30),(34,50),(10,48)],fill=grey)
d.polygon([(22,32),(26,16),(32,32)],fill=back)
d.ellipse((17,35,21,39),fill=(20,20,20))
im.save(out+"/wolf.png")
