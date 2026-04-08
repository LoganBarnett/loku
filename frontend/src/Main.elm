port module Main exposing (main)

import Browser
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Page.Browse as Browse
import Page.Player as Player
import Route exposing (Route(..))
import Url exposing (Url)


port logError : String -> Cmd msg

port videoProgress : (Float -> msg) -> Sub msg


type Page
    = BrowsePage Browse.Model
    | PlayerPage Player.Model
    | NotFoundPage


type alias Flags =
    { canWebm : Bool
    }


type alias Model =
    { key : Nav.Key
    , page : Page
    , canWebm : Bool
    }


type Msg
    = UrlRequested Browser.UrlRequest
    | UrlChanged Url
    | BrowseMsg Browse.Msg
    | PlayerMsg Player.Msg


main : Program Flags Model Msg
main =
    Browser.application
        { init = init
        , view = view
        , update = update
        , subscriptions = \_ -> videoProgress (PlayerMsg << Player.VideoProgress)
        , onUrlRequest = UrlRequested
        , onUrlChange = UrlChanged
        }


init : Flags -> Url -> Nav.Key -> ( Model, Cmd Msg )
init flags url key =
    routeToPage key flags.canWebm (Route.parse url)
        |> Tuple.mapFirst (\page -> { key = key, page = page, canWebm = flags.canWebm })


routeToPage : Nav.Key -> Bool -> Route -> ( Page, Cmd Msg )
routeToPage key canWebm route =
    case route of
        Route.Browse params ->
            Browse.init key params
                |> Tuple.mapBoth BrowsePage (Cmd.map BrowseMsg)

        Route.Player path ->
            Player.init canWebm path
                |> Tuple.mapBoth PlayerPage (Cmd.map PlayerMsg)

        Route.NotFound ->
            ( NotFoundPage, Cmd.none )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        UrlRequested (Browser.Internal url) ->
            ( model, Nav.pushUrl model.key (Url.toString url) )

        UrlRequested (Browser.External url) ->
            ( model, Nav.load url )

        UrlChanged url ->
            case Route.parse url of
                Route.Browse params ->
                    case model.page of
                        BrowsePage subModel ->
                            if Browse.currentPath subModel == params.path then
                                -- Same directory: update query/page without re-fetching.
                                Browse.updateParams params subModel
                                    |> Tuple.mapFirst (\m -> { model | page = BrowsePage m })
                                    |> Tuple.mapSecond (Cmd.map BrowseMsg)

                            else
                                Browse.init model.key params
                                    |> Tuple.mapBoth
                                        (\m -> { model | page = BrowsePage m })
                                        (Cmd.map BrowseMsg)

                        _ ->
                            Browse.init model.key params
                                |> Tuple.mapBoth
                                    (\m -> { model | page = BrowsePage m })
                                    (Cmd.map BrowseMsg)

                route ->
                    routeToPage model.key model.canWebm route
                        |> Tuple.mapFirst (\page -> { model | page = page })

        BrowseMsg subMsg ->
            case model.page of
                BrowsePage subModel ->
                    Browse.update subMsg subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = BrowsePage m })
                            (Cmd.map BrowseMsg)

                _ ->
                    ( model, Cmd.none )

        PlayerMsg (Player.MediaError code) ->
            case model.page of
                PlayerPage subModel ->
                    Player.update (Player.MediaError code) subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = PlayerPage m })
                            (\cmd ->
                                Cmd.batch
                                    [ Cmd.map PlayerMsg cmd
                                    , logError (Player.mediaErrorMessage code)
                                    ]
                            )

                _ ->
                    ( model, Cmd.none )

        PlayerMsg subMsg ->
            case model.page of
                PlayerPage subModel ->
                    Player.update subMsg subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = PlayerPage m })
                            (Cmd.map PlayerMsg)

                _ ->
                    ( model, Cmd.none )


view : Model -> Browser.Document Msg
view model =
    { title = "Loku"
    , body =
        [ div [ style "max-width" "1200px", style "margin" "0 auto" ]
            [ case model.page of
                BrowsePage subModel ->
                    Html.map BrowseMsg (Browse.view subModel)

                PlayerPage subModel ->
                    Html.map PlayerMsg (Player.view subModel)

                NotFoundPage ->
                    p [ style "padding" "2rem" ] [ text "Page not found." ]
            ]
        ]
    }
